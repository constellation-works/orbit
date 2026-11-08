//! Translate a builtin tool call into what a resident cycle's grant must
//! hold for it ([`ResidentCallClass`]); core's cycle guard decides.
//!
//! The mapping is the rights table of the resident mandate-and-authority
//! spec: `dispatch` starts or requeues runs and drains, `promote` moves a
//! task to `backlog`, `merge` lands reviewed work. Filing a `proposed` task,
//! comments, artifacts, friction, decision requests, notices and the
//! checkpoint need no right; everything else that writes is refused.

use orbit_common::governance::decision::DecisionVerb;
use orbit_common::governance::drain::DrainVerb;
use orbit_common::governance::evolution::EvolutionVerb;
use orbit_common::governance::friction::FrictionVerb;
use orbit_common::governance::memory::MemoryVerb;
use orbit_common::governance::notice::NoticeVerb;
use orbit_common::governance::operation::{Effect, OperationVerb};
use orbit_common::governance::resident::ResidentVerb;
use orbit_common::governance::task::TaskVerb;
use orbit_common::governance::workflow::WorkflowVerb;
use orbit_tools::OrbitBuiltinAction;
use orbit_types::workflow::ResidentRight;
use serde_json::Value;

use crate::application::resident::ResidentCallClass;

/// The tool's canonical name, as the action ledger records it.
pub(super) fn resident_guard_tool_name(action: OrbitBuiltinAction) -> &'static str {
    match action {
        OrbitBuiltinAction::CommandExec => "orbit.command.exec",
        OrbitBuiltinAction::DesktopRead => "orbit.desktop.read",
        OrbitBuiltinAction::DesktopAutomation => "orbit.desktop.automation",
        OrbitBuiltinAction::DesktopTaskSnapshot => "orbit.desktop.task.snapshot",
        OrbitBuiltinAction::DesktopTaskWrite => "orbit.desktop.task.write",
        OrbitBuiltinAction::PipelineInvoke => "orbit.pipeline.invoke",
        OrbitBuiltinAction::PipelineWait => "orbit.pipeline.wait",
        OrbitBuiltinAction::AutoTask(verb) => verb.tool_name(),
        OrbitBuiltinAction::Agents(verb) => verb.tool_name(),
        OrbitBuiltinAction::Automation(verb) => verb.tool_name(),
        OrbitBuiltinAction::Attention(verb) => verb.tool_name(),
        OrbitBuiltinAction::Codebase(verb) => verb.tool_name(),
        OrbitBuiltinAction::Decision(verb) => verb.tool_name(),
        OrbitBuiltinAction::Drain(verb) => verb.tool_name(),
        OrbitBuiltinAction::Evolution(verb) => verb.tool_name(),
        OrbitBuiltinAction::Memory(verb) => verb.tool_name(),
        OrbitBuiltinAction::Experiment(verb) => verb.tool_name(),
        OrbitBuiltinAction::Friction(verb) => verb.tool_name(),
        OrbitBuiltinAction::Orchestrator(verb) => verb.tool_name(),
        OrbitBuiltinAction::Search(verb) => verb.tool_name(),
        OrbitBuiltinAction::Task(verb) => verb.tool_name(),
        OrbitBuiltinAction::Workflow(verb) => verb.tool_name(),
        OrbitBuiltinAction::WorkspaceClaim(verb) => verb.tool_name(),
        OrbitBuiltinAction::Notice(verb) => verb.tool_name(),
        OrbitBuiltinAction::Resident(verb) => verb.tool_name(),
    }
}

/// Reads pass; a noun with no resident write passes reads and refuses writes.
fn read_only<V: OperationVerb>(verb: V) -> ResidentCallClass {
    match verb.spec().effect {
        Effect::Read => ResidentCallClass::Read,
        Effect::Write => ResidentCallClass::Refused,
    }
}

fn status(input: &Value) -> Option<&str> {
    input.get("status").and_then(Value::as_str).map(str::trim)
}

fn task_update_class(input: &Value) -> ResidentCallClass {
    // Operator routes and the CLI's status-inferring approval are not a
    // resident's: it names the transition it means.
    if input.get("force").and_then(Value::as_bool) == Some(true)
        || input.get("approve").and_then(Value::as_bool) == Some(true)
    {
        return ResidentCallClass::Refused;
    }
    match status(input) {
        None => ResidentCallClass::Open,
        Some("backlog") => ResidentCallClass::Gated(ResidentRight::Promote),
        Some("in-progress") => ResidentCallClass::Gated(ResidentRight::Dispatch),
        Some("done") => ResidentCallClass::CompleteTask,
        // Refused with the route that does it: a `handoff` checkpoint.
        Some("review") => ResidentCallClass::Handoff,
        Some(_) => ResidentCallClass::Refused,
    }
}

/// What a resident cycle's grant must hold for this call.
pub(super) fn resident_guard_class(action: OrbitBuiltinAction, input: &Value) -> ResidentCallClass {
    use ResidentCallClass::{Gated, Open, Read, Refused};
    use ResidentRight::{Dispatch, Merge, Promote};
    match action {
        OrbitBuiltinAction::PipelineInvoke => Gated(Dispatch),
        OrbitBuiltinAction::PipelineWait
        | OrbitBuiltinAction::DesktopRead
        | OrbitBuiltinAction::DesktopTaskSnapshot => Read,
        OrbitBuiltinAction::CommandExec
        | OrbitBuiltinAction::DesktopAutomation
        | OrbitBuiltinAction::DesktopTaskWrite => Refused,
        OrbitBuiltinAction::Task(verb) => match verb {
            TaskVerb::List
            | TaskVerb::Eligible
            | TaskVerb::Show
            | TaskVerb::ArtifactGet
            | TaskVerb::LocksList => Read,
            // Restoration rewrites selectors/history for one task or a whole
            // sweep. No resident right grants this maintenance operation.
            TaskVerb::Lint => {
                if input.get("restore_pruned").and_then(Value::as_bool) == Some(true) {
                    Refused
                } else {
                    Read
                }
            }
            TaskVerb::Add => match status(input) {
                None | Some("proposed") => Open,
                Some("backlog") => Gated(Promote),
                Some(_) => Refused,
            },
            TaskVerb::Update => task_update_class(input),
            TaskVerb::ArtifactPut => Open,
            TaskVerb::Requeue | TaskVerb::Repilot => Gated(Dispatch),
            TaskVerb::RetryHandoff | TaskVerb::ReconcileReview => Gated(Merge),
            TaskVerb::LocksReserve
            | TaskVerb::LocksRelease
            | TaskVerb::ReviewReset
            | TaskVerb::BlockTriage
            | TaskVerb::RecoverClaim
            | TaskVerb::RevokeHandoff
            | TaskVerb::Reject
            | TaskVerb::Delete => Refused,
        },
        OrbitBuiltinAction::Workflow(verb) => match verb {
            WorkflowVerb::Auto | WorkflowVerb::Ship | WorkflowVerb::RunResume => Gated(Dispatch),
            WorkflowVerb::Land => Gated(Merge),
            WorkflowVerb::RunList | WorkflowVerb::RunObserve | WorkflowVerb::RunShow => Read,
        },
        OrbitBuiltinAction::Drain(verb) => match verb {
            DrainVerb::Probe | DrainVerb::ReceiptLookup | DrainVerb::Claims => Read,
            DrainVerb::Pull | DrainVerb::ClaimBind | DrainVerb::ClaimSettle => Gated(Dispatch),
        },
        OrbitBuiltinAction::Decision(verb) => match verb {
            DecisionVerb::List | DecisionVerb::Show | DecisionVerb::Digest => Read,
            DecisionVerb::ChannelList => Read,
            DecisionVerb::Raise => Open,
            // Answers, reversals and channel administration are the human's.
            DecisionVerb::Answer
            | DecisionVerb::Reverse
            | DecisionVerb::RuleAuto
            | DecisionVerb::Consolidate
            | DecisionVerb::Submit
            | DecisionVerb::ChannelRegister
            | DecisionVerb::ChannelRevoke
            | DecisionVerb::ChannelRotateKey => Refused,
        },
        OrbitBuiltinAction::Memory(verb) => match verb {
            MemoryVerb::Show | MemoryVerb::History => Read,
            // Every crew memory write, and export to host files, is the
            // operator's.
            MemoryVerb::Export | MemoryVerb::Write | MemoryVerb::Reset | MemoryVerb::Copy => {
                Refused
            }
        },
        OrbitBuiltinAction::Notice(verb) => match verb {
            NoticeVerb::List | NoticeVerb::Show => Read,
            NoticeVerb::Post => Open,
            // Closing a maintenance notice would lift the cycle's own
            // observe-only narrowing.
            NoticeVerb::Close => Refused,
        },
        OrbitBuiltinAction::Resident(verb) => match verb {
            ResidentVerb::Show
            | ResidentVerb::List
            | ResidentVerb::Metrics
            | ResidentVerb::Digest => Read,
            ResidentVerb::Checkpoint => Open,
            // Relay and control are the manager's: a cycle never speaks for
            // the human or changes its own firing.
            ResidentVerb::Grant | ResidentVerb::Relay | ResidentVerb::Control => Refused,
        },
        OrbitBuiltinAction::Friction(verb) => match verb {
            FrictionVerb::List | FrictionVerb::Show | FrictionVerb::Stats | FrictionVerb::Tags => {
                Read
            }
            FrictionVerb::Add => Open,
            FrictionVerb::Update | FrictionVerb::Resolve | FrictionVerb::Rehome => Refused,
        },
        OrbitBuiltinAction::Evolution(verb) => match verb {
            EvolutionVerb::List | EvolutionVerb::Show => Read,
            // A cost event is recorded like friction; declining is the operator's.
            EvolutionVerb::Add => Open,
            EvolutionVerb::Decline => Refused,
        },
        OrbitBuiltinAction::AutoTask(verb) => read_only(verb),
        OrbitBuiltinAction::Agents(verb) => read_only(verb),
        OrbitBuiltinAction::Automation(verb) => read_only(verb),
        OrbitBuiltinAction::Attention(verb) => read_only(verb),
        OrbitBuiltinAction::Codebase(verb) => read_only(verb),
        OrbitBuiltinAction::Experiment(verb) => read_only(verb),
        OrbitBuiltinAction::Orchestrator(verb) => read_only(verb),
        OrbitBuiltinAction::Search(verb) => read_only(verb),
        OrbitBuiltinAction::WorkspaceClaim(verb) => read_only(verb),
    }
}
