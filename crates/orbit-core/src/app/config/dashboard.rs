use orbit_common::OrbitError;
use orbit_common::governance::authorization::DASHBOARD_CONFIG_SET;
use serde_json::{Map, Value, json};

use super::Outcome;
use crate::Core;
use crate::app::exec::{DashboardOrigin, DashboardRow, Operation};
use crate::application::config::{
    ConfigScope, ConfigUnsetRefusal, ConfigWriteInit, ConfigWriteOutcome, GuardedConfigUnset,
    delete_crew, set_crew, set_key, unset_key, unset_key_preserving_effective,
};

/// One dashboard configuration mutation.
pub enum DashboardConfigWriteKind {
    /// `PUT /api/config/keys/:key`
    SetKey {
        /// Dotted config key.
        key: String,
        /// Typed JSON value to write.
        value: Value,
        /// Target file scope.
        scope: ConfigScope,
        /// First-write initialization policy.
        init: ConfigWriteInit,
    },
    /// `DELETE /api/config/keys/:key`
    UnsetKey {
        /// Dotted config key.
        key: String,
        /// Target file scope.
        scope: ConfigScope,
    },
    /// `DELETE /api/config/keys/:key?preserve_effective=true`
    UnsetKeyPreservingEffective {
        /// Dotted config key.
        key: String,
        /// Target file scope.
        scope: ConfigScope,
    },
    /// `PUT /api/config/crews/:name`
    SetCrew {
        /// Crew name.
        name: String,
        /// Crew fields to write or clear.
        fields: Map<String, Value>,
        /// Target file scope.
        scope: ConfigScope,
        /// First-write initialization policy.
        init: ConfigWriteInit,
    },
    /// `DELETE /api/config/crews/:name`
    DeleteCrew {
        /// Crew name.
        name: String,
        /// Target file scope.
        scope: ConfigScope,
    },
}

impl DashboardConfigWriteKind {
    /// The key a write targets and the arguments its audit row records.
    fn audited(&self) -> (String, Value) {
        match self {
            DashboardConfigWriteKind::SetKey {
                key, value, scope, ..
            } => (
                key.clone(),
                json!({
                    "key": key,
                    "value": value,
                    "scope": scope.label(),
                }),
            ),
            DashboardConfigWriteKind::UnsetKey { key, scope } => (
                key.clone(),
                json!({
                    "key": key,
                    "value": Value::Null,
                    "scope": scope.label(),
                }),
            ),
            DashboardConfigWriteKind::UnsetKeyPreservingEffective { key, scope } => (
                key.clone(),
                json!({
                    "key": key,
                    "value": Value::Null,
                    "scope": scope.label(),
                    "preserve_effective": true,
                }),
            ),
            DashboardConfigWriteKind::SetCrew {
                name,
                fields,
                scope,
                ..
            } => {
                let target = format!("crews.{name}");
                (
                    target.clone(),
                    json!({
                        "key": target,
                        "value": Value::Object(fields.clone()),
                        "scope": scope.label(),
                    }),
                )
            }
            DashboardConfigWriteKind::DeleteCrew { name, scope } => {
                let target = format!("crews.{name}");
                (
                    target.clone(),
                    json!({
                        "key": target,
                        "value": Value::Null,
                        "scope": scope.label(),
                    }),
                )
            }
        }
    }
}

/// One configuration write request from a dashboard session.
pub struct DashboardConfigWrite {
    /// Which mutation to apply.
    pub kind: DashboardConfigWriteKind,
    /// Hostname recorded on the dashboard audit row.
    pub host: Option<String>,
}

impl DashboardConfigWrite {
    /// The governed operation this write requires and its audit row names.
    pub(super) fn operation(&self) -> Operation {
        let (target, arguments) = self.kind.audited();
        Operation::dashboard_read("config", "dashboard-write")
            .requires(&DASHBOARD_CONFIG_SET)
            .dashboard_row(
                &DASHBOARD_CONFIG_SET,
                target,
                arguments,
                DashboardOrigin {
                    host: self.host.clone(),
                    ..Default::default()
                },
            )
    }
}

/// Outcome of an admitted dashboard configuration write.
pub enum DashboardConfigWriteResult {
    /// The write applied and updated the config file.
    Applied {
        /// Audit/response target key (e.g. `workflow.base_branch` or `crews.foo`).
        target: String,
        /// Applied write details.
        outcome: ConfigWriteOutcome,
    },
    /// A guarded unset refused because removal would change the effective value.
    Refused(ConfigUnsetRefusal),
}

/// Apply one admitted configuration write.
pub(super) fn write(
    core: &Core,
    write: DashboardConfigWrite,
) -> Result<DashboardConfigWriteResult, OrbitError> {
    let runtime = core.runtime();
    let kind = write.kind;
    let (result_target, _) = kind.audited();
    let result = match kind {
        DashboardConfigWriteKind::SetKey {
            key,
            value,
            scope,
            init,
        } => DashboardConfigWriteResult::Applied {
            target: result_target,
            outcome: set_key(runtime, &key, &value, scope, init)?,
        },
        DashboardConfigWriteKind::UnsetKey { key, scope } => DashboardConfigWriteResult::Applied {
            target: result_target,
            outcome: unset_key(runtime, &key, scope)?,
        },
        DashboardConfigWriteKind::UnsetKeyPreservingEffective { key, scope } => {
            match unset_key_preserving_effective(runtime, &key, scope)? {
                GuardedConfigUnset::Unset(outcome) => DashboardConfigWriteResult::Applied {
                    target: result_target,
                    outcome,
                },
                GuardedConfigUnset::Refused(refusal) => {
                    DashboardConfigWriteResult::Refused(refusal)
                }
            }
        }
        DashboardConfigWriteKind::SetCrew {
            name,
            fields,
            scope,
            init,
        } => DashboardConfigWriteResult::Applied {
            target: result_target,
            outcome: set_crew(runtime, &name, &fields, scope, init)?,
        },
        DashboardConfigWriteKind::DeleteCrew { name, scope } => {
            DashboardConfigWriteResult::Applied {
                target: result_target,
                outcome: delete_crew(runtime, &name, scope)?,
            }
        }
    };
    Ok(result)
}

/// A write's audit row: a refused unset is a failure carrying its message,
/// and an applied write records the old and new values.
pub(super) fn row(result: &Result<Outcome, OrbitError>, mut arguments: Value) -> DashboardRow {
    match result {
        Ok(Outcome::DashboardWritten(DashboardConfigWriteResult::Refused(refusal))) => {
            DashboardRow {
                failure: Some(refusal.message.clone()),
                target: None,
                arguments,
            }
        }
        Ok(Outcome::DashboardWritten(DashboardConfigWriteResult::Applied { outcome, .. })) => {
            if let Some(object) = arguments.as_object_mut() {
                object.insert("old_value".to_string(), outcome.old_value.clone());
                object.insert("new_value".to_string(), outcome.new_value.clone());
            }
            DashboardRow {
                failure: None,
                target: None,
                arguments,
            }
        }
        result => DashboardRow::of(result, arguments),
    }
}
