use std::time::Instant;

use orbit_common::OrbitError;
use orbit_common::governance::decision::DecisionVerb;
use orbit_common::governance::operation::OperationVerb;
use orbit_common::governance::task::TaskVerb;
use orbit_tools::{OrbitBuiltinAction, OrbitTaskScope};
use orbit_types::task::{TaskWriteProvenance, TaskWriteSurface};
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::tool::{McpCapability, McpTransport, ToolSessionContext};
use orbit_types::workflow::JobRunTrigger;
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::app::task::{ToolCall, ToolCaller};
use crate::app::tool_call;
use crate::application::agents::AgentDetailSurface;
use crate::application::resident::{ResidentActionTicket, ResidentCallAdmission};
use crate::application::task::{
    TaskMutationAudit, audit_status_for_task_mutation_error, with_task_write_provenance,
};
use crate::runtime::authorization::resolved_caller_capabilities;

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
        TaskVerb::ResumeDelivery => Some("resume_delivery"),
        TaskVerb::BlockTriage => (input.get("action").and_then(Value::as_str)
            != Some("candidates"))
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
    let mut result = execute_admitted(
        runtime,
        task_scope,
        caller,
        action,
        input,
        ticket.as_ref().map(|ticket| (ticket, brokered_resident)),
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
    resident_call: Option<(&ResidentActionTicket, bool)>,
    redaction_report: &mut super::artifact_redaction::ArtifactRedactionReport,
) -> Result<Value, OrbitError> {
    // Only the authenticated broker can substitute the resident ledger
    // for checks on operator-only operations.
    let resident_action_key = resident_call
        .filter(|(_, brokered)| *brokered)
        .map(|(ticket, _)| ticket.resident_action_key());
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
        && resolved_caller_capabilities(runtime.env(), session_context)
            .contains(&McpCapability::Operator);
    let agent_for_audit = agent.clone();
    let model_for_audit = model.clone();
    let mut persisted_task_id = None;
    let mut response = match action {
        OrbitBuiltinAction::AutoTask(verb) => super::auto_task_tools::dispatch(
            &crate::Core::within_tool_entry(runtime.clone()),
            verb,
            input,
            tool_call::Caller::default(),
        ),
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
        OrbitBuiltinAction::Automation(verb) => super::automation_tools::dispatch(
            &crate::Core::within_tool_entry(runtime.clone()),
            verb,
            input,
            tool_call::Caller::default(),
        ),
        OrbitBuiltinAction::Decision(verb) => {
            let provenance = session_write_provenance(session_context);
            let surface = provenance.surface.unwrap_or(TaskWriteSurface::Mcp);
            let caller = provenance.caller.clone();
            let started = Instant::now();
            let result = with_task_write_provenance(provenance, || {
                super::decision_tools::dispatch(
                    &crate::Core::within_tool_entry(runtime.clone()),
                    ToolCall {
                        caller: ToolCaller {
                            session_context,
                            agent,
                            model,
                            reservation_owner,
                        },
                        operator,
                        resident_action_key,
                        resident_ticket: resident_call.map(|(ticket, _)| ticket),
                    },
                    verb,
                    input,
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
            &crate::Core::within_tool_entry(runtime.clone()),
            verb,
            input,
            tool_call::Caller {
                agent,
                model,
                ..tool_call::Caller::default()
            },
        ),
        OrbitBuiltinAction::CommandExec => super::command_tools::exec(
            &crate::Core::within_tool_entry(runtime.clone()),
            input,
            agent,
            model,
        ),
        OrbitBuiltinAction::Drain(verb) => super::drain_tools::dispatch(
            &crate::Core::within_tool_entry(runtime.clone()),
            session_context,
            verb,
            input,
        ),
        // ADR-0209 bearing 1 [ORB-10358]: the friction handler table is the
        // friction app, keyed by the registry's verb enum. This entry already
        // authorized and audited the call.
        OrbitBuiltinAction::Friction(verb) => super::friction_tools::dispatch(
            &crate::Core::within_tool_entry(runtime.clone()),
            verb,
            input,
            model,
        ),
        OrbitBuiltinAction::PipelineInvoke => super::pipeline_tools::invoke(
            &crate::Core::within_tool_entry(runtime.clone()),
            input,
            agent,
            model,
            reservation_owner,
            submission_trigger(session_context),
        ),
        OrbitBuiltinAction::PipelineWait => super::pipeline_tools::wait(
            &crate::Core::within_tool_entry(runtime.clone()),
            input,
            agent,
            model,
        ),
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
            let comment_checkpoint = (verb == TaskVerb::Update)
                .then(|| {
                    input
                        .get("comment")
                        .and_then(Value::as_str)
                        .filter(|comment| {
                            crate::application::resident::is_checkpoint_comment_json(comment)
                        })
                        .map(ToOwned::to_owned)
                })
                .flatten();
            let result = with_task_write_provenance(provenance, || {
                super::task::dispatch(
                    &crate::Core::within_tool_entry(runtime.clone()),
                    ToolCall {
                        caller: ToolCaller {
                            session_context,
                            agent,
                            model,
                            reservation_owner,
                        },
                        operator,
                        resident_action_key,
                        resident_ticket: resident_call.map(|(ticket, _)| ticket),
                    },
                    verb,
                    input,
                )
            });
            if let Some(comment) = comment_checkpoint.as_deref()
                && let Some(target_id) = input_task_id.as_deref()
            {
                let parsed = serde_json::from_str::<Value>(comment).ok();
                match &result {
                    Ok(_) => runtime.clear_resident_checkpoint_refusal_on_accept(
                        target_id,
                        task_scope.run_id.as_deref(),
                        parsed.as_ref(),
                    ),
                    Err(error) => runtime.record_resident_checkpoint_refusal_if_live(
                        target_id,
                        task_scope.run_id.as_deref(),
                        parsed.as_ref(),
                        error,
                    ),
                }
            }
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
                detail.insert("tool".into(), json!(verb.tool_name()));
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
            &crate::Core::within_tool_entry(runtime.clone()),
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
        OrbitBuiltinAction::Evolution(verb) => super::evolution_tools::dispatch(
            &crate::Core::within_tool_entry(runtime.clone()),
            verb,
            input,
            tool_call::Caller::default(),
        ),
        OrbitBuiltinAction::Resident(verb) => super::resident_tools::dispatch(
            &crate::Core::within_tool_entry(runtime.clone()),
            verb,
            input,
            tool_call::Caller {
                session: session_context.clone(),
                run_id: task_scope.run_id.clone(),
                agent,
                model,
            },
        ),
        OrbitBuiltinAction::WorkspaceClaim(verb) => super::workspace_claim_tools::dispatch(
            &crate::Core::within_tool_entry(runtime.clone()),
            verb,
            input,
            agent,
            model,
        ),
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
