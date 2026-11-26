use orbit_common::OrbitError;
use orbit_common::governance::authorization::DASHBOARD_CONFIG_SET;
use serde_json::{Map, Value, json};

use super::Outcome;
use crate::Core;
use crate::app::exec;
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

/// One configuration write request from an authenticated dashboard session.
pub struct DashboardConfigWrite {
    /// Which mutation to apply.
    pub kind: DashboardConfigWriteKind,
    /// Whether the dashboard session holds operator capability.
    pub operator_session: bool,
    /// Hostname recorded on the dashboard audit row.
    pub host: Option<String>,
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

pub(super) fn exec(core: &Core, write: DashboardConfigWrite) -> Result<Outcome, OrbitError> {
    let DashboardConfigWrite {
        kind,
        operator_session,
        host,
    } = write;
    let (target, arguments) = match &kind {
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
    };

    let runtime = core.runtime();
    let result_target = target.clone();
    exec::run_dashboard_with_arguments(
        core,
        exec::DashboardOperation {
            operation: &DASHBOARD_CONFIG_SET,
            target: &target,
            arguments,
            operator_session,
            host,
            workspace: None,
            machine_name: String::new(),
            recorded_as: None,
        },
        move || {
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
                DashboardConfigWriteKind::UnsetKey { key, scope } => {
                    DashboardConfigWriteResult::Applied {
                        target: result_target,
                        outcome: unset_key(runtime, &key, scope)?,
                    }
                }
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
        },
        |result| match result {
            DashboardConfigWriteResult::Applied { .. } => None,
            DashboardConfigWriteResult::Refused(refusal) => Some(refusal.message.clone()),
        },
        |result, mut arguments| {
            if let DashboardConfigWriteResult::Applied { outcome, .. } = result
                && let Some(object) = arguments.as_object_mut()
            {
                object.insert("old_value".to_string(), outcome.old_value.clone());
                object.insert("new_value".to_string(), outcome.new_value.clone());
            }
            arguments
        },
    )
    .map(Outcome::DashboardWritten)
}
