//! The `task.mutation` row a session's task write is owed.

use std::time::Instant;

use orbit_types::task::TaskWriteSurface;
use orbit_types::telemetry::AuditEventStatus;

use super::{DashboardWrite, Outcome};
use crate::app::exec::SessionAudit;
use crate::application::task::{TaskMutationAudit, audit_status_for_task_mutation_error};

/// The writer of `write`'s mutation row: its verb, target, caller and detail,
/// with the role given or the one resolved for the model. A write that named
/// no target is recorded against the record it wrote.
pub(super) fn writer(write: &DashboardWrite) -> SessionAudit<Outcome> {
    let verb = write.verb;
    let named_target = write.target_id.clone();
    let caller = write.caller.clone();
    let role = write.role.clone();
    let model = write.model.clone();
    let detail = write.detail.clone();
    let started = Instant::now();
    Box::new(move |core, result| {
        let runtime = core.runtime();
        let duration_ms = i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX);
        let role = role
            .unwrap_or_else(|| runtime.resolve_task_mutation_audit_role(None, model.as_deref()));
        let (target_id, status, error_message) = match result {
            Ok(outcome) => (
                written_id(outcome).or(named_target),
                AuditEventStatus::Success,
                None,
            ),
            Err(error) => (
                named_target,
                audit_status_for_task_mutation_error(error),
                Some(error.to_string()),
            ),
        };
        runtime.record_task_mutation_audit(TaskMutationAudit {
            verb,
            target_id: target_id.as_deref(),
            role: &role,
            surface: TaskWriteSurface::Web,
            caller: caller.as_deref(),
            status,
            duration_ms,
            error_message,
            detail,
            session_context: None,
        });
    })
}

/// The record `outcome` wrote, when the command could not name it.
fn written_id(outcome: &Outcome) -> Option<String> {
    match outcome {
        Outcome::Screened(write) => Some(write.written.task.id.clone()),
        Outcome::Created(id) => Some(id.clone()),
        Outcome::DecisionRequest(request) => Some(request.task_id.clone()),
        _ => None,
    }
}
