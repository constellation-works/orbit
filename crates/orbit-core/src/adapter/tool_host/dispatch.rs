use std::time::Instant;

use orbit_common::OrbitError;
use orbit_common::governance::decision::DecisionVerb;
use orbit_common::governance::task::TaskVerb;
use orbit_tools::{OrbitBuiltinAction, OrbitTaskScope, ReservationOwnerContext};
use orbit_types::task::{TaskWriteProvenance, TaskWriteSurface};
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::tool::{McpCapability, McpTransport, ToolSessionContext};
use orbit_types::workflow::JobRunTrigger;
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::agents::AgentDetailSurface;
use crate::application::resident::ResidentCallAdmission;
use crate::application::task::{
    TaskMutationAudit, audit_status_for_task_mutation_error, with_task_write_provenance,
};
use crate::runtime::authorization::resolved_caller_capabilities;

/// Everything the dispatch table knows about *who* is making this call.
///
/// Grouped rather than passed as four more parameters because the handlers
/// consume different subsets of it: attribution for a persisted record, the
/// reservation owner for a lock write, and the session itself for the one
/// operation whose decision depends on the caller [ORB-11354].
pub(super) struct ToolCaller<'a> {
    pub(super) session_context: &'a ToolSessionContext,
    pub(super) agent: Option<String>,
    pub(super) model: Option<String>,
    pub(super) reservation_owner: Option<ReservationOwnerContext>,
}

/// Provenance for a run this call submits [ORB-13016].
///
/// The MCP adapter mints an origin session id for every session it serves;
/// `orbit tool run` and in-process callers carry none, so they stay `cli`.
fn submission_trigger(session_context: &ToolSessionContext) -> JobRunTrigger {
    if session_context.origin_session_id.is_some() {
        JobRunTrigger::mcp()
    } else {
        JobRunTrigger::cli()
    }
}

/// Native commands have no transport; `tool run` supplies a local one.
/// MCP (including local stdio) always mints an origin session id.
fn agent_detail_surface(session: &ToolSessionContext) -> AgentDetailSurface {
    if session.origin_session_id.is_some() || session.transport == Some(McpTransport::SshMcp) {
        AgentDetailSurface::Mcp
    } else if session.transport.is_some() {
        AgentDetailSurface::ToolRun
    } else {
        AgentDetailSurface::Cli
    }
}

fn session_write_provenance(session: &ToolSessionContext) -> TaskWriteProvenance {
    let surface = session.write_surface.unwrap_or_else(|| {
        if session.origin_session_id.is_some() || session.transport == Some(McpTransport::SshMcp) {
            TaskWriteSurface::Mcp
        } else {
            TaskWriteSurface::Cli
        }
    });
    let caller = match surface {
        TaskWriteSurface::Mcp => session.resolved_session_caller(),
        TaskWriteSurface::Cli | TaskWriteSurface::Web => session
            .write_caller
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .or_else(|| session.remote_caller_machine_id().map(ToOwned::to_owned)),
    };
    TaskWriteProvenance::new(Some(surface), caller)
}

fn task_verb_mutation_name(verb: TaskVerb, input: &Value) -> Option<&'static str> {
    match verb {
        TaskVerb::Add => Some("add"),
        TaskVerb::Update => Some("update"),
        TaskVerb::ArtifactPut => Some("artifact_put"),
        TaskVerb::ReviewReset => Some("review_reset"),
        TaskVerb::Repilot => Some("repilot"),
        TaskVerb::Requeue => Some("requeue"),
        TaskVerb::BlockTriage => (input.get("action").and_then(Value::as_str) != Some("candidates"))
            .then_some("block_triage"),
        TaskVerb::ReconcileReview => Some("reconcile_review"),
        TaskVerb::RetryHandoff => Some("retry_handoff"),
        TaskVerb::RecoverClaim => Some("recover_claim"),
        TaskVerb::RevokeHandoff => Some("revoke_handoff"),
        TaskVerb::Reject => Some("reject"),
        TaskVerb::Delete => Some("delete"),
        TaskVerb::Lint => (input.get("restore_pruned").and_then(Value::as_bool) == Some(true))
            .then_some("lint_restore_pruned"),
        TaskVerb::ArtifactGet
        | TaskVerb::LocksList
        | TaskVerb::LocksReserve
        | TaskVerb::LocksRelease
        | TaskVerb::List
        | TaskVerb::Eligible
        | TaskVerb::Show => None,
    }
}

pub(super) fn execute(
    runtime: &OrbitRuntime,
    task_scope: &OrbitTaskScope,
    brokered_resident: bool,
    caller: ToolCaller<'_>,
    action: OrbitBuiltinAction,
    input: Value,
) -> Result<Value, OrbitError> {
    let (input, mut redaction_report) =
        super::artifact_redaction::sanitize_tool_input(action, input)?;
    let ticket = match runtime.admit_resident_cycle_call(
        task_scope.run_id.as_deref(),
        brokered_resident,
        super::resident_guard::resident_guard_tool_name(action),
        super::resident_guard::resident_guard_class(action, &input),
        &input,
    )? {
        ResidentCallAdmission::Proceed => None,
        ResidentCallAdmission::Ledgered(ticket) => Some(ticket),
        ResidentCallAdmission::AlreadyDone(answer) => return Ok(answer),
    };
    // The cycle guard ledgers a right-gated write only for a live cycle that
    // holds the right, so a ledgered call stands in for the operator check
    // the handler would otherwise repeat.
    let resident_cycle_admitted = brokered_resident && ticket.is_some();
    let mut result = execute_admitted(
        runtime,
        task_scope,
        caller,
        action,
        input,
        resident_cycle_admitted,
        &mut redaction_report,
    );
    if let Some(ticket) = ticket {
        runtime.close_resident_action(ticket, &mut result)?;
    }
    result
}

fn execute_admitted(
    runtime: &OrbitRuntime,
    task_scope: &OrbitTaskScope,
    caller: ToolCaller<'_>,
    action: OrbitBuiltinAction,
    input: Value,
    resident_cycle_admitted: bool,
    redaction_report: &mut super::artifact_redaction::ArtifactRedactionReport,
) -> Result<Value, OrbitError> {
    let ToolCaller {
        session_context,
        agent,
        model,
        reservation_owner,
    } = caller;
    if let Some(mut result) =
        super::worker_tools::execute(runtime, session_context, action, &input)?
    {
        super::artifact_redaction::finish_tool_response(
            runtime,
            action,
            &mut result,
            redaction_report,
            session_context
                .worker_invocation
                .as_ref()
                .map(|binding| binding.task_id.as_str()),
            agent.as_deref(),
            model.as_deref(),
        )?;
        return Ok(result);
    }
    // An operator is a caller holding operator capability and naming no
    // agent identity; only such a caller can close out a rescued blocked
    // task without starting work on it.
    let operator = agent.is_none()
        && model.is_none()
        && resolved_caller_capabilities(session_context).contains(&McpCapability::Operator);
    let agent_for_audit = agent.clone();
    let model_for_audit = model.clone();
    let mut persisted_task_id = None;
    let mut response = match action {
        OrbitBuiltinAction::AutoTask(verb) => {
            super::auto_task_tools::dispatch(runtime, verb, input)
        }
        OrbitBuiltinAction::DesktopAutomation => {
            super::desktop::automation::control(runtime, input, submission_trigger(session_context))
        }
        OrbitBuiltinAction::DesktopRead => {
            super::desktop::task::read(runtime, session_context, input)
        }
        OrbitBuiltinAction::DesktopTaskSnapshot => {
            super::desktop::task::snapshot(runtime, session_context, input)
        }
        OrbitBuiltinAction::DesktopTaskWrite => {
            let provenance = session_write_provenance(session_context);
            let surface = provenance.surface.unwrap_or(TaskWriteSurface::Mcp);
            let caller = provenance.caller.clone();
            let started = Instant::now();
            let result = with_task_write_provenance(provenance, || {
                super::desktop::task::write(runtime, session_context, input, agent, model)
            });
            if session_context.transport.is_some()
                || session_context.origin_session_id.is_some()
                || session_context.write_surface.is_some()
            {
                let target_id = result
                    .as_ref()
                    .ok()
                    .and_then(|value| value.get("id").and_then(Value::as_str))
                    .map(ToOwned::to_owned);
                let (status, error_message) = match &result {
                    Ok(_) => (AuditEventStatus::Success, None),
                    Err(error) => (
                        audit_status_for_task_mutation_error(error),
                        Some(error.to_string()),
                    ),
                };
                let role = runtime.resolve_task_mutation_audit_role(
                    agent_for_audit.as_deref(),
                    model_for_audit.as_deref(),
                );
                runtime.record_task_mutation_audit(TaskMutationAudit {
                    verb: "desktop_write",
                    target_id: target_id.as_deref(),
                    role: &role,
                    surface,
                    caller: caller.as_deref(),
                    status,
                    duration_ms: started.elapsed().as_millis() as i64,
                    error_message,
                    detail: json!({"tool": "orbit.desktop.task.write"}),
                    session_context: Some(session_context),
                });
            }
            result
        }
        OrbitBuiltinAction::Agents(verb) => super::agents_tools::dispatch(
            runtime,
            verb,
            input,
            agent_detail_surface(session_context),
        ),
        OrbitBuiltinAction::Attention(_) => {
            let resident_scope =
                runtime.resident_cycle_attention_scope(task_scope.run_id.as_deref());
            let mint = input.get("wait_grant").and_then(serde_json::Value::as_bool) == Some(true);
            crate::application::attention::attention_query(
                runtime,
                input,
                session_context,
                resident_scope.as_ref(),
            )
            .and_then(|response| {
                if mint {
                    crate::application::attention::with_wait_grant(runtime, response)
                } else {
                    Ok(response)
                }
            })
        }
        OrbitBuiltinAction::Codebase(verb) => super::codebase_tools::dispatch(runtime, verb, input),
        OrbitBuiltinAction::Automation(verb) => {
            super::automation_tools::dispatch(runtime, verb, input)
        }
        OrbitBuiltinAction::Decision(verb) => {
            let provenance = session_write_provenance(session_context);
            let surface = provenance.surface.unwrap_or(TaskWriteSurface::Mcp);
            let caller = provenance.caller.clone();
            let started = Instant::now();
            let result = with_task_write_provenance(provenance, || {
                super::decision_tools::dispatch(
                    runtime,
                    verb,
                    input,
                    session_context,
                    agent.as_deref(),
                    model.as_deref(),
                )
            });
            if matches!(verb, DecisionVerb::Submit | DecisionVerb::Raise)
                && (session_context.transport.is_some()
                    || session_context.origin_session_id.is_some()
                    || session_context.write_surface.is_some())
            {
                let target_id = result
                    .as_ref()
                    .ok()
                    .and_then(|value| value.get("task_id").and_then(Value::as_str))
                    .map(ToOwned::to_owned);
                let (status, error_message) = match &result {
                    Ok(_) => (AuditEventStatus::Success, None),
                    Err(error) => (
                        audit_status_for_task_mutation_error(error),
                        Some(error.to_string()),
                    ),
                };
                let role = runtime.resolve_task_mutation_audit_role(
                    agent_for_audit.as_deref(),
                    model_for_audit.as_deref(),
                );
                let verb_label = match verb {
                    DecisionVerb::Submit => "decision_submit",
                    _ => "decision_raise",
                };
                runtime.record_task_mutation_audit(TaskMutationAudit {
                    verb: verb_label,
                    target_id: target_id.as_deref(),
                    role: &role,
                    surface,
                    caller: caller.as_deref(),
                    status,
                    duration_ms: started.elapsed().as_millis() as i64,
                    error_message,
                    detail: json!({"tool": "orbit.decision"}),
                    session_context: Some(session_context),
                });
            }
            result
        }
        OrbitBuiltinAction::Experiment(verb) => super::experiment_tools::dispatch(
            runtime,
            verb,
            input,
            agent.as_deref(),
            model.as_deref(),
        ),
        OrbitBuiltinAction::CommandExec => super::command_tools::exec(runtime, input, agent, model),
        OrbitBuiltinAction::Drain(verb) => {
            super::drain_tools::dispatch(runtime, session_context, verb, input)
        }
        // ADR-0209 bearing 1 [ORB-10358]: the friction handler table lives with
        // the other friction handlers, keyed by the registry's verb enum.
        OrbitBuiltinAction::Friction(verb) => {
            super::friction_tools::dispatch(runtime, verb, input, model)
        }
        OrbitBuiltinAction::PipelineInvoke => super::pipeline_tools::invoke(
            runtime,
            input,
            agent,
            model,
            reservation_owner,
            submission_trigger(session_context),
        ),
        OrbitBuiltinAction::PipelineWait => {
            super::pipeline_tools::wait(runtime, input, agent, model)
        }
        OrbitBuiltinAction::Search(verb) => super::search_tools::dispatch(runtime, verb, input),
        OrbitBuiltinAction::Task(verb) => {
            let provenance = session_write_provenance(session_context);
            let surface = provenance.surface.unwrap_or(TaskWriteSurface::Cli);
            let caller = provenance.caller.clone();
            let mutation_verb = task_verb_mutation_name(verb, &input);
            let input_task_id = input
                .get("id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            let requested_status = input
                .get("status")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            let should_audit_mutation = mutation_verb.is_some()
                && (session_context.transport.is_some()
                    || session_context.origin_session_id.is_some()
                    || session_context.write_surface.is_some());
            let started = Instant::now();
            let result = with_task_write_provenance(provenance, || {
                super::task::operation::dispatch(
                    runtime,
                    ToolCaller {
                        session_context,
                        agent,
                        model,
                        reservation_owner,
                    },
                    operator,
                    resident_cycle_admitted,
                    verb,
                    input,
                )
            });
            if let Some(mutation_verb) = mutation_verb
                && should_audit_mutation
            {
                let (status, error_message, resolved_target) = match &result {
                    Ok((_, written_id)) => (
                        AuditEventStatus::Success,
                        None,
                        written_id.clone().or(input_task_id),
                    ),
                    Err(error) => (
                        audit_status_for_task_mutation_error(error),
                        Some(error.to_string()),
                        input_task_id,
                    ),
                };
                let role = runtime.resolve_task_mutation_audit_role(
                    agent_for_audit.as_deref(),
                    model_for_audit.as_deref(),
                );
                let mut detail = serde_json::Map::new();
                detail.insert("tool".into(), json!(verb.spec().tool_name));
                if let Some(requested_status) = requested_status {
                    detail.insert("status".into(), json!(requested_status));
                }
                runtime.record_task_mutation_audit(TaskMutationAudit {
                    verb: mutation_verb,
                    target_id: resolved_target.as_deref(),
                    role: &role,
                    surface,
                    caller: caller.as_deref(),
                    status,
                    duration_ms: started.elapsed().as_millis() as i64,
                    error_message,
                    detail: Value::Object(detail),
                    session_context: Some(session_context),
                });
            }
            let (response, written) = result?;
            persisted_task_id = written;
            Ok(response)
        }
        OrbitBuiltinAction::Workflow(verb) => super::workflow_tools::dispatch(
            runtime,
            session_context,
            verb,
            input,
            agent,
            model,
            submission_trigger(session_context),
        ),
        OrbitBuiltinAction::Notice(verb) => super::notice_tools::dispatch(
            runtime,
            task_scope,
            session_context,
            verb,
            input,
            model.as_deref(),
        ),
        OrbitBuiltinAction::Memory(verb) => super::memory_tools::dispatch(
            runtime,
            task_scope,
            session_context,
            verb,
            input,
            model.as_deref(),
        ),
        OrbitBuiltinAction::Evolution(verb) => {
            super::evolution_tools::dispatch(runtime, verb, input)
        }
        OrbitBuiltinAction::Resident(verb) => super::resident_tools::dispatch(
            runtime,
            session_context,
            verb,
            input,
            agent.as_deref(),
            model.as_deref(),
        ),
        OrbitBuiltinAction::WorkspaceClaim(verb) => {
            super::workspace_claim_tools::dispatch(runtime, verb, input, agent, model)
        }
    }?;
    super::artifact_redaction::finish_tool_response(
        runtime,
        action,
        &mut response,
        redaction_report,
        persisted_task_id.as_deref(),
        agent_for_audit.as_deref(),
        model_for_audit.as_deref(),
    )?;
    Ok(response)
}
