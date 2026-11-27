//! The `dashboard.operations` row of a governed dashboard write.

use std::time::Instant;

use orbit_common::OrbitError;
use orbit_common::observability::audit_id::audit_execution_id;
use orbit_store::AuditEventInsertParams;
use orbit_types::telemetry::AuditEventStatus;
use serde_json::Value;

use crate::app::exec::operation::DashboardRowSpec;
use crate::app::exec::{App, Session};
use crate::{Core, Error};

/// What a dashboard operation's one audit row records once it has run.
pub(crate) struct DashboardRow {
    /// Why the row is a failure; a success without one.
    pub(crate) failure: Option<String>,
    /// The target, where the outcome names another than the request did.
    pub(crate) target: Option<String>,
    pub(crate) arguments: Value,
}

impl DashboardRow {
    /// The row of an outcome that says nothing more than its error: a
    /// failure carrying the error's text, a success otherwise.
    pub(crate) fn of<T>(result: &Result<T, Error>, arguments: Value) -> Self {
        Self {
            failure: result.as_ref().err().map(ToString::to_string),
            target: None,
            arguments,
        }
    }
}

/// One declared dashboard row, held from before admission until the command
/// ends.
pub(in crate::app::exec) struct DashboardAudit {
    session: Session,
    spec: DashboardRowSpec,
    started: Instant,
}

impl DashboardAudit {
    pub(super) fn start(session: Session, spec: DashboardRowSpec) -> Self {
        Self {
            session,
            spec,
            started: Instant::now(),
        }
    }

    /// Write the row. A refused session is always recorded, as denied; an
    /// admitted command records what its app says the outcome owes, which
    /// may be nothing.
    pub(super) fn finish<A: App>(self, core: &Core, result: &Result<A::Outcome, Error>) {
        let Self {
            session,
            spec,
            started,
        } = self;
        let (status, provenance, capabilities, recorded) = match result {
            Err(OrbitError::AuthorizationDenied(denial)) => (
                AuditEventStatus::Denied,
                denial.provenance,
                Default::default(),
                DashboardRow {
                    failure: Some(denial.to_string()),
                    target: None,
                    arguments: spec.arguments,
                },
            ),
            result => {
                let Some(recorded) = A::dashboard_row(result, &spec.target, spec.arguments) else {
                    return;
                };
                let caller = session.capabilities();
                (
                    if recorded.failure.is_some() {
                        AuditEventStatus::Failure
                    } else {
                        AuditEventStatus::Success
                    },
                    caller.provenance(),
                    caller.grants().clone(),
                    recorded,
                )
            }
        };
        let runtime = core.runtime();
        let workspace = spec.origin.workspace.unwrap_or_else(|| {
            runtime
                .workspace_id()
                .unwrap_or_else(|_| runtime.shared_root().display().to_string())
        });
        let requested_target = spec.target;
        let recorded_as = spec.recorded_as.unwrap_or(spec.governed.id);
        let row = AuditEventInsertParams {
            subcommand: Some(provenance.to_string()),
            target_type: Some(recorded_as.to_string()),
            target_id: Some(recorded.target.unwrap_or_else(|| requested_target.clone())),
            role: "operator".to_string(),
            status,
            exit_code: i32::from(status != AuditEventStatus::Success),
            duration_ms: i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX),
            working_directory: runtime
                .workspace_runtime_binding()
                .map(|binding| binding.repo_root.display().to_string())
                .unwrap_or_else(|| runtime.shared_root().display().to_string()),
            arguments_json: serde_json::to_string(&recorded.arguments).ok(),
            error_message: recorded.failure,
            host: spec.origin.host,
            workspace_id: Some(workspace),
            caller_machine_name: Some(spec.origin.machine_name.clone()),
            process_machine_name: Some(spec.origin.machine_name),
            effective_capabilities: capabilities,
            ..super::unattributed_row(
                audit_execution_id("dashboard-operations"),
                "dashboard.operations",
            )
        };
        if let Err(error) = runtime.record_audit_event(&row) {
            tracing::error!(operation = recorded_as, target = requested_target, %error,
                "failed to persist dashboard operation audit");
        }
    }
}
