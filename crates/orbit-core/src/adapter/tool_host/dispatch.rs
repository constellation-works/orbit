use orbit_common::OrbitError;
use orbit_tools::{OrbitBuiltinAction, OrbitTaskScope, ReservationOwnerContext};
use orbit_types::tool::{McpCapability, McpTransport, ToolSessionContext};
use orbit_types::workflow::JobRunTrigger;
use serde_json::Value;

use crate::OrbitRuntime;
use crate::application::agents::AgentDetailSurface;
use crate::application::resident::ResidentCallAdmission;
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

pub(super) fn execute(
    runtime: &OrbitRuntime,
    task_scope: &OrbitTaskScope,
    caller: ToolCaller<'_>,
    action: OrbitBuiltinAction,
    input: Value,
) -> Result<Value, OrbitError> {
    let (input, mut redaction_report) =
        super::artifact_redaction::sanitize_tool_input(action, input)?;
    // A resident cycle's agent reaches tools only within its cycle's grant;
    // the trusted run binding, never input, says whether this is one.
    let ticket = match runtime.admit_resident_cycle_call(
        task_scope.run_id.as_deref(),
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
    redaction_report: &mut super::artifact_redaction::ArtifactRedactionReport,
) -> Result<Value, OrbitError> {
    let ToolCaller {
        session_context,
        agent,
        model,
        reservation_owner,
    } = caller;
    if let Some(mut result) =
        super::worker_tools::execute(runtime, session_context, action, &input, model.as_deref())?
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
            super::desktop::task::write(runtime, session_context, input, agent, model)
        }
        OrbitBuiltinAction::Agents(verb) => super::agents_tools::dispatch(
            runtime,
            verb,
            input,
            agent_detail_surface(session_context),
        ),
        OrbitBuiltinAction::Attention(_) => {
            crate::application::attention::attention_query(runtime, input, session_context)
        }
        OrbitBuiltinAction::Codebase(verb) => super::codebase_tools::dispatch(runtime, verb, input),
        OrbitBuiltinAction::Automation(verb) => {
            super::automation_tools::dispatch(runtime, verb, input)
        }
        OrbitBuiltinAction::Decision(verb) => super::decision_tools::dispatch(
            runtime,
            verb,
            input,
            session_context,
            agent.as_deref(),
            model.as_deref(),
        ),
        OrbitBuiltinAction::Experiment(verb) => super::experiment_tools::dispatch(
            runtime,
            verb,
            input,
            agent.as_deref(),
            model.as_deref(),
        ),
        OrbitBuiltinAction::Orchestrator(verb) => super::orchestrator_tools::dispatch(
            runtime,
            verb,
            input,
            agent_detail_surface(session_context),
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
            let (response, written) = super::task::operation::dispatch(
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
        OrbitBuiltinAction::Notice(verb) => super::notice_tools::dispatch(
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
        OrbitBuiltinAction::Lease(verb) => {
            super::lease_tools::dispatch(runtime, verb, input, agent, model)
        }
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
