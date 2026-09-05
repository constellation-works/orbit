use orbit_common::OrbitError;
use orbit_tools::{OrbitBuiltinAction, OrbitTaskScope, ReservationOwnerContext};
use serde_json::Value;

use crate::OrbitRuntime;

pub(super) fn execute(
    runtime: &OrbitRuntime,
    task_scope: &OrbitTaskScope,
    action: OrbitBuiltinAction,
    input: Value,
    agent: Option<String>,
    model: Option<String>,
    reservation_owner: Option<ReservationOwnerContext>,
) -> Result<Value, OrbitError> {
    let (input, redaction_report) = super::artifact_redaction::sanitize_tool_input(action, input)?;
    let agent_for_audit = agent.clone();
    let model_for_audit = model.clone();
    let mut response = match action {
        OrbitBuiltinAction::AdrAdd
        | OrbitBuiltinAction::AdrShow
        | OrbitBuiltinAction::AdrList
        | OrbitBuiltinAction::AdrRestore
        | OrbitBuiltinAction::AdrUpdate
        | OrbitBuiltinAction::AdrSupersede => Err(OrbitError::InvalidInput(
            "ADR lifecycle tools have been retired; edit docs/design/**/4_decisions.md".to_string(),
        )),
        OrbitBuiltinAction::AutoTaskAdd => super::auto_task_tools::add(runtime, input),
        OrbitBuiltinAction::AutoTaskList => super::auto_task_tools::list(runtime, input),
        OrbitBuiltinAction::AutoTaskMint => super::auto_task_tools::mint(runtime, input),
        OrbitBuiltinAction::AutoTaskShow => super::auto_task_tools::show(runtime, input),
        OrbitBuiltinAction::AutoTaskUpdate => super::auto_task_tools::update(runtime, input),
        OrbitBuiltinAction::AutoTaskToggle => super::auto_task_tools::toggle(runtime, input),
        OrbitBuiltinAction::CommandExec => super::command_tools::exec(runtime, input, agent, model),
        OrbitBuiltinAction::DocsList => super::docs_tools::list(runtime, input),
        OrbitBuiltinAction::DocsShow => super::docs_tools::show(runtime, input),
        OrbitBuiltinAction::DocsAdd => super::docs_tools::add(runtime, input),
        OrbitBuiltinAction::DocsIndex => super::docs_tools::index(runtime, input),
        OrbitBuiltinAction::DocsMigrate => super::docs_tools::migrate(runtime, input),
        // ADR-0209 bearing 1 [ORB-10358]: the friction handler table lives with
        // the other friction handlers, keyed by the registry's verb enum.
        OrbitBuiltinAction::Friction(verb) => {
            super::friction_tools::dispatch(runtime, verb, input, model)
        }
        OrbitBuiltinAction::PipelineInvoke => {
            super::pipeline_tools::invoke(runtime, input, agent, model, reservation_owner)
        }
        OrbitBuiltinAction::PipelineWait => {
            super::pipeline_tools::wait(runtime, input, agent, model)
        }
        OrbitBuiltinAction::Search => super::search_tools::search(runtime, input),
        OrbitBuiltinAction::SemanticIndex => super::semantic_tools::index(runtime, input),
        OrbitBuiltinAction::SemanticInstall => super::semantic_tools::install(runtime, input),
        OrbitBuiltinAction::SemanticStats => super::semantic_tools::stats(runtime),
        OrbitBuiltinAction::SemanticUninstall => super::semantic_tools::uninstall(runtime, input),
        OrbitBuiltinAction::StateGet => super::state_tools::get(task_scope, input),
        OrbitBuiltinAction::StateSet => super::state_tools::set(task_scope, input),
        OrbitBuiltinAction::TaskAdd => super::task_tools::add(runtime, input, agent, model),
        OrbitBuiltinAction::TaskApprove => super::task_tools::approve(runtime, input, agent, model),
        OrbitBuiltinAction::TaskDelete => super::task_tools::delete(runtime, input),
        OrbitBuiltinAction::TaskLint => super::task_tools::lint(runtime, input),
        OrbitBuiltinAction::TaskList => super::task_tools::list(runtime, input),
        OrbitBuiltinAction::TaskLocks => crate::runtime::task::locks::list(runtime),
        OrbitBuiltinAction::TaskLocksRelease => {
            crate::runtime::task::locks::release(runtime, input, agent, model)
        }
        OrbitBuiltinAction::TaskLocksReserve => {
            crate::runtime::task::locks::reserve(runtime, input, agent, model, reservation_owner)
        }
        OrbitBuiltinAction::TaskReject => super::task_tools::reject(runtime, input, agent, model),
        OrbitBuiltinAction::TaskShow => super::task_tools::show(runtime, input),
        OrbitBuiltinAction::TaskStart => super::task_tools::start(runtime, input, agent, model),
        OrbitBuiltinAction::TaskUpdate => super::task_tools::update(runtime, input, agent, model),
        OrbitBuiltinAction::WorkflowShip => {
            super::workflow_tools::ship(runtime, input, agent, model)
        }
        OrbitBuiltinAction::WorkflowRunShow => super::workflow_tools::show(runtime, input),
        OrbitBuiltinAction::WorkflowRunList => super::workflow_tools::list(runtime, input),
        OrbitBuiltinAction::WorkflowRunResume => {
            super::workflow_tools::resume(runtime, input, agent, model)
        }
        OrbitBuiltinAction::WorkflowRunWorkers => {
            super::workflow_tools::workers(runtime, input, agent, model)
        }
        OrbitBuiltinAction::WorkspaceClaimAcquire => {
            crate::runtime::workspace_claim::acquire(runtime, input, agent, model)
        }
        OrbitBuiltinAction::WorkspaceClaimRelease => {
            crate::runtime::workspace_claim::release(runtime, input, agent, model)
        }
        OrbitBuiltinAction::WorkspaceClaimShow => crate::runtime::workspace_claim::show(runtime),
    }?;
    super::artifact_redaction::finish_tool_response(
        runtime,
        action,
        &mut response,
        &redaction_report,
        agent_for_audit.as_deref(),
        model_for_audit.as_deref(),
    )?;
    Ok(response)
}
