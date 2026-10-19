use orbit_common::OrbitError;
use orbit_tools::{OrbitBuiltinAction, OrbitTaskScope, ReservationOwnerContext};
use orbit_types::tool::{McpCapability, ToolSessionContext};
use orbit_types::workflow::JobRunTrigger;
use serde_json::Value;

use crate::OrbitRuntime;
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

pub(super) fn execute(
    runtime: &OrbitRuntime,
    _task_scope: &OrbitTaskScope,
    caller: ToolCaller<'_>,
    action: OrbitBuiltinAction,
    input: Value,
) -> Result<Value, OrbitError> {
    let ToolCaller {
        session_context,
        agent,
        model,
        reservation_owner,
    } = caller;
    let (input, mut redaction_report) =
        super::artifact_redaction::sanitize_tool_input(action, input)?;
    if let Some(mut result) =
        super::worker_tools::execute(runtime, session_context, action, &input, model.as_deref())?
    {
        super::artifact_redaction::finish_tool_response(
            runtime,
            action,
            &mut result,
            &mut redaction_report,
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
        OrbitBuiltinAction::AgentInvoke => {
            super::agent_tools::invoke(runtime, session_context, input, agent, model)
        }
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
            super::desktop::task::write(runtime, session_context, input, agent, model)
        }
        OrbitBuiltinAction::Agents(verb) => super::agents_tools::dispatch(runtime, verb, input),
        OrbitBuiltinAction::Codebase(verb) => super::codebase_tools::dispatch(runtime, verb, input),
        OrbitBuiltinAction::Orchestrator(verb) => {
            super::orchestrator_tools::dispatch(runtime, verb, input)
        }
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
            let (response, written) = super::task_operation_tools::dispatch(
                runtime,
                ToolCaller {
                    session_context,
                    agent,
                    model,
                    reservation_owner,
                },
                operator,
                verb,
                input,
            )?;
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
        OrbitBuiltinAction::WorkspaceClaim(verb) => {
            super::workspace_claim_tools::dispatch(runtime, verb, input, agent, model)
        }
    }?;
    super::artifact_redaction::finish_tool_response(
        runtime,
        action,
        &mut response,
        &mut redaction_report,
        persisted_task_id.as_deref(),
        agent_for_audit.as_deref(),
        model_for_audit.as_deref(),
    )?;
    Ok(response)
}
