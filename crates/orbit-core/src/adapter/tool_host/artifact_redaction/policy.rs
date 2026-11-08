use orbit_common::governance::agents::AgentsVerb;
use orbit_common::governance::auto_task::AutoTaskVerb;
use orbit_common::governance::automation::AutomationVerb;
use orbit_common::governance::codebase::CodebaseVerb;
use orbit_common::governance::decision::DecisionVerb;
use orbit_common::governance::drain::DrainVerb;
use orbit_common::governance::experiment::ExperimentVerb;
use orbit_common::governance::memory::MemoryVerb;
use orbit_common::governance::operation::{OperationVerb, ParamSpec, Redaction};
use orbit_common::governance::orchestrator::OrchestratorVerb;
use orbit_common::governance::search::SearchVerb;
use orbit_common::governance::task::{TASK_CLI_OPTIONS_PARAM, TaskVerb};
use orbit_common::governance::workflow::WorkflowVerb;
use orbit_common::governance::workspace_claim::WorkspaceClaimVerb;
use orbit_tools::OrbitBuiltinAction;

#[derive(Clone, Copy)]
pub(super) enum TextMode {
    Free,
    PathOnly,
}

#[derive(Clone, Copy)]
pub(super) struct NestedArrayPolicy {
    pub(super) array_key: &'static str,
    pub(super) field_key: &'static str,
    pub(super) field_alias: Option<&'static str>,
    pub(super) mode: TextMode,
}

#[derive(Clone, Copy)]
pub(super) struct NestedObjectPolicy {
    pub(super) object_key: &'static str,
    pub(super) free_text_fields: &'static [&'static str],
    pub(super) free_text_arrays: &'static [&'static str],
}

pub(super) struct ActionPolicy {
    /// A registry operation's parameter groups; each parameter is redacted
    /// per its declared [`Redaction`] class, in declaration order.
    pub(super) declared: &'static [&'static [ParamSpec]],
    pub(super) free_text_fields: &'static [&'static str],
    pub(super) free_text_arrays: &'static [&'static str],
    pub(super) path_fields: &'static [&'static str],
    pub(super) path_arrays: &'static [&'static str],
    pub(super) nested_arrays: &'static [NestedArrayPolicy],
    pub(super) nested_objects: &'static [NestedObjectPolicy],
}

const NO_REDACTION: ActionPolicy = ActionPolicy {
    declared: &[],
    free_text_fields: &[],
    free_text_arrays: &[],
    path_fields: &[],
    path_arrays: &[],
    nested_arrays: &[],
    nested_objects: &[],
};

/// The free text of each option an agent raises on a decision request.
/// A memory write's file text is free prose that persists in the audit row.
const MEMORY_WRITE_FILES: &[NestedArrayPolicy] = &[NestedArrayPolicy {
    array_key: "files",
    field_key: "text",
    field_alias: None,
    mode: TextMode::Free,
}];

const DECISION_RAISE_OPTIONS: &[NestedArrayPolicy] = &[
    NestedArrayPolicy {
        array_key: "options",
        field_key: "label",
        field_alias: None,
        mode: TextMode::Free,
    },
    NestedArrayPolicy {
        array_key: "options",
        field_key: "consequence",
        field_alias: None,
        mode: TextMode::Free,
    },
];

/// The free text among `orbit task add`'s CLI options.
const TASK_ADD_CLI_OPTIONS: &[NestedObjectPolicy] = &[NestedObjectPolicy {
    object_key: TASK_CLI_OPTIONS_PARAM,
    free_text_fields: &["plan"],
    free_text_arrays: &[],
}];

/// A template replacement and a field-scoped template edit carry the same
/// free-text fields.
const AUTO_TASK_TEMPLATE: &[NestedObjectPolicy] = &[
    NestedObjectPolicy {
        object_key: "template",
        free_text_fields: &["title", "description"],
        free_text_arrays: &["acceptance_criteria"],
    },
    NestedObjectPolicy {
        object_key: "template_patch",
        free_text_fields: &["title", "description"],
        free_text_arrays: &["acceptance_criteria"],
    },
];

/// Every builtin action must make an explicit redaction decision here.
///
/// Keeping this match exhaustive makes an added action a compile failure until
/// its persisted input fields have been reviewed. `NO_REDACTION` is deliberate
/// for read-only actions and mutations that only persist structural values.
pub(super) fn policy_for_action(action: OrbitBuiltinAction) -> ActionPolicy {
    match action {
        // Claim-lifecycle payloads are compared against durable replay records;
        // all parameters are structural and must stay byte-for-byte intact.
        OrbitBuiltinAction::Drain(verb) => match verb {
            DrainVerb::Probe
            | DrainVerb::ReceiptLookup
            | DrainVerb::Pull
            | DrainVerb::ClaimBind
            | DrainVerb::ClaimSettle
            | DrainVerb::Claims => ActionPolicy {
                declared: verb.spec().params,
                ..NO_REDACTION
            },
        },
        OrbitBuiltinAction::WorkspaceClaim(verb) => match verb {
            WorkspaceClaimVerb::Acquire
            | WorkspaceClaimVerb::Release
            | WorkspaceClaimVerb::Show => ActionPolicy {
                declared: verb.spec().params,
                ..NO_REDACTION
            },
        },
        OrbitBuiltinAction::Friction(verb) => ActionPolicy {
            declared: verb.spec().params,
            ..NO_REDACTION
        },
        // A notice's title, body and close reason are free text quoted into
        // other agents' prompts; each parameter redacts per its declared class.
        OrbitBuiltinAction::Notice(verb) => ActionPolicy {
            declared: verb.spec().params,
            ..NO_REDACTION
        },
        // A grant's quote is free text carried to the human verbatim; each
        // parameter redacts per its declared class.
        OrbitBuiltinAction::Evolution(verb) => ActionPolicy {
            declared: verb.spec().params,
            nested_objects: &[NestedObjectPolicy {
                object_key: "event",
                free_text_fields: &["what", "wished"],
                free_text_arrays: &[],
            }],
            ..NO_REDACTION
        },
        OrbitBuiltinAction::Resident(verb) => ActionPolicy {
            declared: verb.spec().params,
            ..NO_REDACTION
        },
        OrbitBuiltinAction::Task(verb) => match verb {
            // Artifact bytes are stored as given; the store screens their path.
            TaskVerb::ArtifactPut => NO_REDACTION,
            // The plan `orbit task add` carries among its CLI options is free
            // text too.
            TaskVerb::Add => ActionPolicy {
                declared: verb.spec().params,
                nested_objects: TASK_ADD_CLI_OPTIONS,
                ..NO_REDACTION
            },
            // Each parameter redacts per its declared class; the task reads
            // persist nothing, and lint writes only recorded selectors. A
            // reconciliation's `command` selects an already-authorized stored
            // command, so it is declared plain and kept byte-for-byte; its
            // serialized reports are redacted at the response boundary.
            TaskVerb::Update
            | TaskVerb::List
            | TaskVerb::Eligible
            | TaskVerb::Show
            | TaskVerb::Lint
            | TaskVerb::ArtifactGet
            | TaskVerb::LocksList
            | TaskVerb::LocksReserve
            | TaskVerb::LocksRelease
            | TaskVerb::ReviewReset
            | TaskVerb::Repilot
            | TaskVerb::Requeue
            | TaskVerb::BlockTriage
            | TaskVerb::ReconcileReview
            | TaskVerb::RetryHandoff
            | TaskVerb::RecoverClaim
            | TaskVerb::RevokeHandoff
            | TaskVerb::Reject
            | TaskVerb::Delete => ActionPolicy {
                declared: verb.spec().params,
                ..NO_REDACTION
            },
        },
        OrbitBuiltinAction::AutoTask(verb) => match verb {
            AutoTaskVerb::Add | AutoTaskVerb::Update => ActionPolicy {
                declared: verb.spec().params,
                nested_objects: AUTO_TASK_TEMPLATE,
                ..NO_REDACTION
            },
            // Delete, recover and reset reasons are operator audit notes kept
            // verbatim; the other verbs persist only structural values.
            AutoTaskVerb::List
            | AutoTaskVerb::Show
            | AutoTaskVerb::Toggle
            | AutoTaskVerb::Delete
            | AutoTaskVerb::Restore
            | AutoTaskVerb::Mint
            | AutoTaskVerb::Recover
            | AutoTaskVerb::Reset => NO_REDACTION,
        },
        // Run selectors, crews and drain settings are structural; a resize
        // reason is an operator audit note kept verbatim.
        OrbitBuiltinAction::Search(verb) => match verb {
            SearchVerb::Query => ActionPolicy { declared: verb.spec().params, ..NO_REDACTION },
        },
        OrbitBuiltinAction::Codebase(verb) => match verb {
            CodebaseVerb::Analytics | CodebaseVerb::Show | CodebaseVerb::Related => ActionPolicy {
                declared: verb.spec().params,
                ..NO_REDACTION
            },
        },
        // Free-text hypothesis and reason redact per their declared class.
        OrbitBuiltinAction::Experiment(verb) => match verb {
            ExperimentVerb::Create
            | ExperimentVerb::Update
            | ExperimentVerb::Clone
            | ExperimentVerb::Start
            | ExperimentVerb::Pause
            | ExperimentVerb::Resume
            | ExperimentVerb::Assign
            | ExperimentVerb::Reweight
            | ExperimentVerb::Conclude
            | ExperimentVerb::Abandon
            | ExperimentVerb::Delete
            | ExperimentVerb::Show
            | ExperimentVerb::List
            | ExperimentVerb::Readout => ActionPolicy {
                declared: verb.spec().params,
                ..NO_REDACTION
            },
        },
        // Crew memory: a write's file text redacts as free text, as does a
        // proposal's declared free-text note; every other verb carries only
        // keys, revisions, a directory and a reason.
        OrbitBuiltinAction::Memory(verb) => match verb {
            MemoryVerb::Write => ActionPolicy {
                declared: verb.spec().params,
                nested_arrays: MEMORY_WRITE_FILES,
                ..NO_REDACTION
            },
            MemoryVerb::Show
            | MemoryVerb::History
            | MemoryVerb::Export
            | MemoryVerb::Reset
            | MemoryVerb::Copy
            | MemoryVerb::Proposals
            | MemoryVerb::ProposalAccept
            | MemoryVerb::ProposalReject
            | MemoryVerb::Propose => ActionPolicy {
                declared: verb.spec().params,
                ..NO_REDACTION
            },
        },
        // List and show persist nothing; the answer verb never reaches
        // dispatch as a tool, and its note redacts per its declared class.
        OrbitBuiltinAction::Decision(verb) => match verb {
            // A raised option's label and consequence persist in task history,
            // so they redact beside the declared question.
            DecisionVerb::Raise => ActionPolicy {
                declared: verb.spec().params,
                nested_arrays: DECISION_RAISE_OPTIONS,
                ..NO_REDACTION
            },
            DecisionVerb::List
            | DecisionVerb::Show
            | DecisionVerb::Answer
            | DecisionVerb::Reverse
            | DecisionVerb::RuleAuto
            | DecisionVerb::Consolidate
            | DecisionVerb::Submit
            | DecisionVerb::Digest
            | DecisionVerb::ChannelRegister
            | DecisionVerb::ChannelList
            | DecisionVerb::ChannelRevoke
            | DecisionVerb::ChannelRotateKey => ActionPolicy {
                declared: verb.spec().params,
                ..NO_REDACTION
            },
        },
        OrbitBuiltinAction::Agents(verb) => match verb {
            AgentsVerb::Scorecard | AgentsVerb::Show => ActionPolicy {
                declared: verb.spec().params,
                ..NO_REDACTION
            },
        },
        OrbitBuiltinAction::Automation(verb) => match verb {
            AutomationVerb::Explain | AutomationVerb::ClearStall => ActionPolicy {
                declared: verb.spec().params,
                ..NO_REDACTION
            },
        },
        OrbitBuiltinAction::Orchestrator(verb) => match verb {
            OrchestratorVerb::Scorecard | OrchestratorVerb::Show => ActionPolicy {
                declared: verb.spec().params,
                ..NO_REDACTION
            },
        },
        OrbitBuiltinAction::Workflow(verb) => match verb {
            WorkflowVerb::Auto
            | WorkflowVerb::Land
            | WorkflowVerb::RunList
            | WorkflowVerb::RunResume
            | WorkflowVerb::RunObserve
            | WorkflowVerb::RunShow
            | WorkflowVerb::Ship => ActionPolicy {
                declared: verb.spec().params,
                ..NO_REDACTION
            },
        },
        OrbitBuiltinAction::DesktopRead
        | OrbitBuiltinAction::DesktopAutomation
        | OrbitBuiltinAction::DesktopTaskSnapshot
        // Desktop writes redact at their transactional persistence boundary.
        | OrbitBuiltinAction::DesktopTaskWrite
        | OrbitBuiltinAction::CommandExec
        | OrbitBuiltinAction::PipelineInvoke
        | OrbitBuiltinAction::PipelineWait
        // [ORB-11354] The invocation prompt is free text and *is* persisted on
        // the run record, but redacting it would change the instruction the
        // agent receives — an operator debugging an auth failure needs to name
        // the thing they are debugging. It stays verbatim; the run record is
        // already operator-only, and the provider argv/stdout paths keep their
        // own redactors.
        | OrbitBuiltinAction::Attention(_) => NO_REDACTION,
    }
}

pub(super) fn is_covered_mutating_action(action: OrbitBuiltinAction) -> bool {
    match action {
        // A registry operation reports redactions when it declares a redacted
        // parameter.
        OrbitBuiltinAction::Friction(verb) => verb
            .spec()
            .params()
            .any(|param| param.redact != Redaction::None),
        OrbitBuiltinAction::AutoTask(verb) => {
            !policy_for_action(OrbitBuiltinAction::AutoTask(verb))
                .nested_objects
                .is_empty()
        }
        OrbitBuiltinAction::Drain(verb) => match verb {
            DrainVerb::Probe
            | DrainVerb::ReceiptLookup
            | DrainVerb::Pull
            | DrainVerb::ClaimBind
            | DrainVerb::ClaimSettle
            | DrainVerb::Claims => false,
        },
        OrbitBuiltinAction::WorkspaceClaim(verb) => match verb {
            WorkspaceClaimVerb::Acquire
            | WorkspaceClaimVerb::Release
            | WorkspaceClaimVerb::Show => false,
        },
        OrbitBuiltinAction::Search(verb) => match verb {
            SearchVerb::Query => false,
        },
        OrbitBuiltinAction::Codebase(verb) => match verb {
            CodebaseVerb::Analytics | CodebaseVerb::Show | CodebaseVerb::Related => false,
        },
        OrbitBuiltinAction::Agents(verb) => match verb {
            AgentsVerb::Scorecard | AgentsVerb::Show => false,
        },
        OrbitBuiltinAction::Automation(verb) => match verb {
            AutomationVerb::Explain | AutomationVerb::ClearStall => false,
        },
        OrbitBuiltinAction::Orchestrator(verb) => match verb {
            OrchestratorVerb::Scorecard | OrchestratorVerb::Show => false,
        },
        OrbitBuiltinAction::Workflow(verb) => match verb {
            WorkflowVerb::Auto
            | WorkflowVerb::Land
            | WorkflowVerb::RunList
            | WorkflowVerb::RunResume
            | WorkflowVerb::RunObserve
            | WorkflowVerb::RunShow
            | WorkflowVerb::Ship => false,
        },
        OrbitBuiltinAction::Task(verb) => match verb {
            TaskVerb::Add
            | TaskVerb::Update
            | TaskVerb::ArtifactPut
            | TaskVerb::ReviewReset
            | TaskVerb::Repilot
            | TaskVerb::Requeue
            | TaskVerb::BlockTriage
            | TaskVerb::ReconcileReview
            | TaskVerb::RetryHandoff
            | TaskVerb::RecoverClaim
            | TaskVerb::RevokeHandoff
            | TaskVerb::Reject => true,
            TaskVerb::List
            | TaskVerb::Eligible
            | TaskVerb::Show
            | TaskVerb::Lint
            | TaskVerb::ArtifactGet
            | TaskVerb::LocksList
            | TaskVerb::LocksReserve
            | TaskVerb::LocksRelease
            | TaskVerb::Delete => false,
        },
        _ => false,
    }
}
