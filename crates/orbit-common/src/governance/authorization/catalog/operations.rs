use orbit_types::tool::McpCapability;

use super::types::{
    DASHBOARD_AUTO_DRAIN_APPROVE_PROPOSED, DASHBOARD_AUTO_DRAIN_AUTO_FIX,
    DASHBOARD_AUTO_DRAIN_COMPLETE, DASHBOARD_AUTO_DRAIN_MODE, DASHBOARD_AUTO_DRAIN_STOP,
    DASHBOARD_AUTO_TASK_DELETE, DASHBOARD_AUTO_TASK_MINT, DASHBOARD_AUTO_TASK_SCHEDULE,
    DASHBOARD_AUTO_TASK_TOGGLE, DASHBOARD_CLAIM_RECOVER, DASHBOARD_CLOCK_CADENCE,
    DASHBOARD_CLOCK_SERVICE, DASHBOARD_CONFIG_SET, DASHBOARD_EXPERIMENT_CONCLUDE,
    DASHBOARD_EXPERIMENT_CREATE, DASHBOARD_EXPERIMENT_START, DASHBOARD_EXPERIMENT_UPDATE,
    DASHBOARD_HANDOFF_APPROVE, DASHBOARD_HANDOFF_RETRY, DASHBOARD_HANDOFF_REVOKE,
    DASHBOARD_HOST_EDIT, DASHBOARD_HOST_FORWARD, DASHBOARD_JOB_RUN, DASHBOARD_LEASE_WRITE,
    DASHBOARD_PLUGIN_DISABLE, DASHBOARD_PLUGIN_ENABLE, DASHBOARD_RESIDENT_WRITE,
    DASHBOARD_ROUTINE_TOGGLE, DASHBOARD_ROUTINE_TRIGGER, DASHBOARD_SCRIPT_GRANT,
    DASHBOARD_TASK_PILOT_ADMISSION, DASHBOARD_TASK_POST_DEPLOY, DASHBOARD_TASK_REQUEUE,
    DASHBOARD_TASK_SETTLE_MERGED, GovernedOperation, OperationSurface, PLUGIN_TOOL_AGENT_CALLABLE,
    PLUGIN_TOOL_MUTATING, PLUGIN_TOOL_READ_ONLY, RESIDENT_GRANT_OPERATION_ID,
    TASK_GRANT_TOOLS_OPERATION_ID,
};

/// Every governed operation, declared exactly once.
///
/// This is the single enumerable place the required capability lives. A call
/// site never names a capability; it names an operation and the chokepoint
/// resolves the requirement here.
///
/// Three rules govern what belongs on this list:
///
/// 1. **Out-of-scope destruction, not all destruction.** The pipeline's own git
///    path — worktree force-removal, `branch -D`, `git clean -fd`, `checkout -B`,
///    `merge --ff-only`, PR merge — is destructive by design and is exactly what
///    a run exists to do. Those operations are not listed: an agent inside a
///    sanctioned run must retain them, and gating them would break every ship.
/// 2. **`Runner` where a run legitimately performs it.** An operation a run
///    reaches through its own dispatcher lists [`McpCapability::Runner`]
///    alongside `Operator`; the run stamps that grant onto the tool context it
///    builds, so the sanction travels with the run rather than with ambient
///    process state.
/// 3. **An identification floor, where answering an unidentified caller is the
///    accident.** A row that lists [`McpCapability::Agent`] is not an operator
///    gate; it says every ordinary caller may perform the operation and a
///    caller this process cannot identify at all may not. The distributed
///    drain's read-only surface is the case [ORB-12582]: a follower holds
///    `agent` and nothing more, so `agent` is exactly who must reach it, while
///    a session that asserts no capability gets no answer about the owner's
///    workspace. Writing the floor here rather than as a session read inside
///    the application function is what makes the CLI and MCP answers identical
///    — only the chokepoint can see the process envelope a CLI caller's
///    authority actually lives in.
pub const GOVERNED_OPERATIONS: &[GovernedOperation] = &[
    GovernedOperation {
        id: "orbit.attention",
        surface: OperationSurface::Tool,
        allowed: &[McpCapability::Agent, McpCapability::Operator],
        rationale: "attention reads task-level operator waits",
    },
    GovernedOperation {
        id: "application consent",
        surface: OperationSurface::CliCommand,
        allowed: &[McpCapability::Operator],
        rationale: "issuing application credentials requires explicit operator consent",
    },
    GovernedOperation {
        id: "application rotate",
        surface: OperationSurface::CliCommand,
        allowed: &[McpCapability::Operator],
        rationale: "rotating application authority requires the operator",
    },
    GovernedOperation {
        id: "application revoke",
        surface: OperationSurface::CliCommand,
        allowed: &[McpCapability::Operator],
        rationale: "revoking application authority requires the operator",
    },
    // Fleet notices: an identification floor. Poster class, rights and
    // limits are decided in core from the caller's session.
    GovernedOperation {
        id: "orbit.notice.post",
        surface: OperationSurface::Tool,
        allowed: &[
            McpCapability::Agent,
            McpCapability::Operator,
            McpCapability::Runner,
        ],
        rationale: "posting a fleet notice writes into other agents' briefs, so the caller must be identified; core derives the poster class and its rights",
    },
    GovernedOperation {
        id: "orbit.notice.close",
        surface: OperationSurface::Tool,
        allowed: &[
            McpCapability::Agent,
            McpCapability::Operator,
            McpCapability::Runner,
        ],
        rationale: "closing a fleet notice changes what the fleet sees, so the caller must be identified; core decides which notices the poster class may close",
    },
    GovernedOperation {
        id: "orbit.resident.checkpoint",
        surface: OperationSurface::Tool,
        allowed: &[
            McpCapability::Agent,
            McpCapability::Operator,
            McpCapability::Runner,
        ],
        rationale: "resident memory is a governed task write; core validates the record and fences it against the mandate's live cycle",
    },
    GovernedOperation {
        id: "orbit.evolution.decline",
        surface: OperationSurface::Tool,
        allowed: &[McpCapability::Operator],
        rationale: "suppressing evolution proposals is an operator decision",
    },
    // Resident authority: an identification floor. Core applies a widening
    // only for an operator caller; any other caller's widening becomes a
    // resident_authority decision request a human must answer.
    GovernedOperation {
        id: RESIDENT_GRANT_OPERATION_ID,
        surface: OperationSurface::Tool,
        allowed: &[
            McpCapability::Agent,
            McpCapability::Operator,
            McpCapability::Runner,
        ],
        rationale: "changing a resident's authority decides what it may do unattended, so the caller must be identified; only an operator's widening applies directly, anyone else's becomes a decision request for the human",
    },
    GovernedOperation {
        id: "orbit.resident.relay",
        surface: OperationSurface::Tool,
        allowed: &[
            McpCapability::Agent,
            McpCapability::Operator,
            McpCapability::Runner,
        ],
        rationale: "a relay puts a human's words in front of a resident, so the relaying session must be identified and recorded; core refuses any input that names authority",
    },
    GovernedOperation {
        id: "orbit.resident.control",
        surface: OperationSurface::Tool,
        allowed: &[
            McpCapability::Agent,
            McpCapability::Operator,
            McpCapability::Runner,
        ],
        rationale: "pausing, stopping or taking over a resident changes what runs unattended, so the caller must be identified and recorded; every action narrows or restores firing under the existing authority",
    },
    GovernedOperation {
        id: "orbit.task.retry_handoff",
        surface: OperationSurface::Tool,
        allowed: &[McpCapability::Operator],
        rationale: "retrying an authorized stopped landing is a deliberate owner operator decision",
    },
    GovernedOperation {
        id: "orbit.task.recover_claim",
        surface: OperationSurface::Tool,
        allowed: &[McpCapability::Operator],
        rationale: "recovery fences a live attempt on another host and moves its task; there is no heartbeat, so only an operator decides the attempt is over",
    },
    GovernedOperation {
        id: "orbit.task.revoke_handoff",
        surface: OperationSurface::Tool,
        allowed: &[McpCapability::Operator],
        rationale: "revocation cancels a pending landing request for work already handed off; only an operator decides that",
    },
    GovernedOperation {
        id: "orbit.task.reconcile_review",
        surface: OperationSurface::Tool,
        allowed: &[McpCapability::Operator],
        rationale: "reconciliation produces the owner-held review and validation evidence that lets a changed merged head complete",
    },
    GovernedOperation {
        id: "orbit.task.repilot",
        surface: OperationSurface::Tool,
        allowed: &[McpCapability::Operator],
        rationale: "only an explicit operator request may repeat a task pilot assessment",
    },
    GovernedOperation {
        id: "orbit.task.requeue",
        surface: OperationSurface::Tool,
        allowed: &[McpCapability::Operator],
        rationale: "requeuing a blocked task overrides a recorded escalation and preserves its published candidate",
    },
    // Field-level, not a tool: `orbit.task.add` and `orbit.task.update`
    // additionally require it when their input carries `granted_tools`.
    GovernedOperation {
        id: TASK_GRANT_TOOLS_OPERATION_ID,
        surface: OperationSurface::Tool,
        allowed: &[McpCapability::Operator],
        rationale: "granting a task tools its implementing activity denies widens what that agent may do, so only an operator may set or change `granted_tools`",
    },
    GovernedOperation {
        id: "orbit.task.review_reset",
        surface: OperationSurface::Tool,
        allowed: &[McpCapability::Operator],
        rationale: "resetting a review budget overrides a recorded admission refusal",
    },
    GovernedOperation {
        id: "orbit.pipeline.invoke",
        surface: OperationSurface::Tool,
        allowed: &[McpCapability::Operator, McpCapability::Runner],
        rationale: "public pipeline submission requires an operator; sanctioned runners retain only their host-authorized child admission path",
    },
    GovernedOperation {
        id: "orbit.workflow.auto",
        surface: OperationSurface::Tool,
        allowed: &[McpCapability::Operator],
        rationale: "auto-drain observation and controls require an operator session",
    },
    GovernedOperation {
        id: "orbit.routine.control",
        surface: OperationSurface::Tool,
        allowed: &[McpCapability::Operator],
        rationale: "routine observation and controls require an operator session",
    },
    GovernedOperation {
        id: "orbit.workflow.land",
        surface: OperationSurface::Tool,
        allowed: &[McpCapability::Operator],
        rationale: "landing merges a delivery pull request and completes its task",
    },
    GovernedOperation {
        id: "orbit.workflow.ship",
        surface: OperationSurface::Tool,
        allowed: &[McpCapability::Operator],
        rationale: "dispatching a ship workflow creates managed runs and worktrees",
    },
    GovernedOperation {
        id: "orbit.workflow.run.observe",
        surface: OperationSurface::Tool,
        allowed: &[McpCapability::Operator],
        rationale: "workflow run observation is an operator surface",
    },
    GovernedOperation {
        id: "orbit.workflow.run.show",
        surface: OperationSurface::Tool,
        allowed: &[McpCapability::Operator],
        rationale: "workflow run observation is an operator surface",
    },
    GovernedOperation {
        id: "orbit.workflow.run.list",
        surface: OperationSurface::Tool,
        allowed: &[McpCapability::Operator],
        rationale: "workflow run observation is an operator surface",
    },
    GovernedOperation {
        id: "orbit.workflow.run.resume",
        surface: OperationSurface::Tool,
        allowed: &[McpCapability::Operator],
        rationale: "resuming a workflow creates another managed run",
    },
    GovernedOperation {
        id: "orbit.command.exec",
        surface: OperationSurface::Tool,
        allowed: &[McpCapability::Operator],
        rationale: "command execution reaches the machine's shell surface through an explicit argv, not a filtered allowlist",
    },
    GovernedOperation {
        id: "orbit.drain.claims",
        surface: OperationSurface::Tool,
        allowed: &[McpCapability::Operator],
        rationale: "execution-claim inspection is the operator's recovery surface: it names every \
                    in-flight attempt, the machine running it, and its landing state",
    },
    // Rule 3 above: an identification floor, not an operator gate. A follower's
    // session holds `agent` and must reach both tools; a caller the chokepoint
    // resolves to nothing gets no answer about the owner's workspace
    // [ORB-12582].
    GovernedOperation {
        id: "orbit.drain.probe",
        surface: OperationSurface::Tool,
        allowed: &[McpCapability::Agent, McpCapability::Operator],
        rationale: "the owner's admission preflight answers for this workspace — its identity, \
                    binary, ship contract, and review policy — so the caller has to be someone \
                    this process can name",
    },
    // [ORB-13625] The mutating half follows the same floor: a follower's drain
    // holds `agent`, and what fences an attempt is the claim journal, which
    // compares every write against the session's own machine and the claim's
    // current phase. Approval, revocation and recovery are not tools at all.
    GovernedOperation {
        id: "orbit.task.pull",
        surface: OperationSurface::Tool,
        allowed: &[McpCapability::Agent, McpCapability::Operator],
        rationale: "pull admission claims a backlog task for the calling machine, so the caller \
                    has to be someone this process can name; the claim itself grants only that \
                    one attempt's execution",
    },
    GovernedOperation {
        id: "orbit.drain.claim.bind",
        surface: OperationSurface::Tool,
        allowed: &[McpCapability::Agent, McpCapability::Operator],
        rationale: "binding a leaf run starts an admitted attempt; the claim journal refuses any \
                    machine but the one the claim was admitted to",
    },
    GovernedOperation {
        id: "orbit.drain.claim.settle",
        surface: OperationSurface::Tool,
        allowed: &[McpCapability::Agent, McpCapability::Operator],
        rationale: "settlement records an attempt's handoff or failure; the claim journal fences \
                    it to the admitted machine and bound run, and a handoff still needs owner \
                    approval before anything lands",
    },
    GovernedOperation {
        id: "orbit.drain.receipt.lookup",
        surface: OperationSurface::Tool,
        allowed: &[McpCapability::Agent, McpCapability::Operator],
        rationale: "receipt reconciliation reads a caller's own admission history on the owner, \
                    so the caller has to be someone this process can name",
    },
    GovernedOperation {
        id: "orbit.auto_task.delete",
        surface: OperationSurface::Tool,
        allowed: &[McpCapability::Operator],
        rationale: "deleting an auto-task discards its scheduler cursor and delivery consumer \
                    state, and a deleted shipped default stays out of later reseeds",
    },
    GovernedOperation {
        id: "orbit.automation.clear_stall",
        surface: OperationSurface::Tool,
        allowed: &[McpCapability::Operator],
        rationale: "clearing a condition stall lets a storm, loop or oscillation fire again; \
                    only an operator may decide it was looked at",
    },
    GovernedOperation {
        id: "orbit.auto_task.reset",
        surface: OperationSurface::Tool,
        allowed: &[McpCapability::Operator],
        rationale: "resetting a delivery consumer forgets its coverage debt and can abandon an \
                    executing action",
    },
    GovernedOperation {
        id: "orbit.task.delete",
        surface: OperationSurface::Tool,
        allowed: &[McpCapability::Operator],
        rationale: "deleting a task destroys its history and artifacts",
    },
    GovernedOperation {
        id: "orbit.task.reject",
        surface: OperationSurface::Tool,
        allowed: &[McpCapability::Operator],
        rationale: "rejecting a task is a human disposition, not an agent one",
    },
    GovernedOperation {
        id: "orbit.task.locks.release",
        surface: OperationSurface::Tool,
        allowed: &[McpCapability::Operator, McpCapability::Runner],
        rationale: "releasing another run's reservation can let two runs edit the same files",
    },
    GovernedOperation {
        id: "orbit.task.locks.reserve",
        surface: OperationSurface::Tool,
        allowed: &[McpCapability::Operator, McpCapability::Runner],
        rationale: "a reservation blocks every other caller from its surface until it expires or \
                    is released, so creating one requires the same authority as removing one — \
                    otherwise a caller could gate others out of a surface it is not itself \
                    trusted to clear",
    },
    GovernedOperation {
        id: "orbit.workspace.claim.release",
        surface: OperationSurface::Tool,
        allowed: &[McpCapability::Operator],
        rationale: "force-releasing a workspace claim displaces the operator currently driving dispatch",
    },
    // The delegated-human decision channel (design
    // `docs/design/human-decision-channel/2_design.md` §3.3). The row is
    // operator-only for every capability-resolved caller — an agent session
    // is refused here. A plugin backend's callback session holds no
    // capability this registry can name, so the runtime chokepoint admits
    // an *identified* callback past this row instead, and the submit
    // handler's channel-binding and token checks decide it: the callback
    // allowlist (the plugin must be granted this tool) and the registration
    // (this channel must be bound to that plugin) are both re-read per
    // call, so the bypass never widens past the operator's registration.
    GovernedOperation {
        id: "orbit.decision.submit",
        surface: OperationSurface::Tool,
        allowed: &[McpCapability::Operator],
        rationale: "submitting a decision applies a human's answer to held work, so only the operator testing a channel — or the registered channel's own plugin callback, which the handler binds to the registration — may call it; an agent session never answers for a human",
    },
    // The digest half of the same channel: it raises the approval
    // requests a transport puts in front of a human. Same row shape and
    // same callback admission as `orbit.decision.submit` — the handler
    // re-reads the registration and binds the caller to it per call.
    GovernedOperation {
        id: "orbit.decision.digest",
        surface: OperationSurface::Tool,
        allowed: &[McpCapability::Operator],
        rationale: "the decision digest raises approval requests for a human to answer, so only the operator — or the registered channel's own plugin callback, which the handler binds to the registration — may call it; an agent session never raises requests for a human",
    },
    // Experiments: every write is the operator's; agents may show and list.
    GovernedOperation {
        id: "orbit.experiment.create",
        surface: OperationSurface::Tool,
        allowed: &[McpCapability::Operator],
        rationale: "defining an experiment decides which treatments tasks may be randomized into, so only an operator may do it",
    },
    GovernedOperation {
        id: "orbit.experiment.update",
        surface: OperationSurface::Tool,
        allowed: &[McpCapability::Operator],
        rationale: "editing an experiment's definition changes the treatments tasks may be randomized into, so only an operator may do it",
    },
    GovernedOperation {
        id: "orbit.experiment.clone",
        surface: OperationSurface::Tool,
        allowed: &[McpCapability::Operator],
        rationale: "cloning an experiment defines a new set of treatments tasks may be randomized into, so only an operator may do it",
    },
    GovernedOperation {
        id: "orbit.experiment.start",
        surface: OperationSurface::Tool,
        allowed: &[McpCapability::Operator],
        rationale: "starting an experiment begins randomizing tasks into its treatments, so only an operator may do it",
    },
    GovernedOperation {
        id: "orbit.experiment.pause",
        surface: OperationSurface::Tool,
        allowed: &[McpCapability::Operator],
        rationale: "pausing an experiment stops it enrolling tasks, so only an operator may do it",
    },
    GovernedOperation {
        id: "orbit.experiment.resume",
        surface: OperationSurface::Tool,
        allowed: &[McpCapability::Operator],
        rationale: "resuming an experiment restarts randomizing tasks into its treatments, so only an operator may do it",
    },
    GovernedOperation {
        id: "orbit.experiment.assign",
        surface: OperationSurface::Tool,
        allowed: &[McpCapability::Operator],
        rationale: "forcing an experiment arm chooses a task's treatment by hand, so only an operator may do it",
    },
    GovernedOperation {
        id: "orbit.experiment.reweight",
        surface: OperationSurface::Tool,
        allowed: &[McpCapability::Operator],
        rationale: "reweighting an experiment changes how tasks are randomized and opens a new segment, so only an operator may do it",
    },
    GovernedOperation {
        id: "orbit.experiment.conclude",
        surface: OperationSurface::Tool,
        allowed: &[McpCapability::Operator],
        rationale: "concluding an experiment records the operator's decision on its treatments, so only an operator may do it",
    },
    GovernedOperation {
        id: "orbit.experiment.abandon",
        surface: OperationSurface::Tool,
        allowed: &[McpCapability::Operator],
        rationale: "abandoning an experiment ends it without a decision, so only an operator may do it",
    },
    GovernedOperation {
        id: "orbit.experiment.delete",
        surface: OperationSurface::Tool,
        allowed: &[McpCapability::Operator],
        rationale: "deleting an experiment removes its record and event log for good, so only an operator may do it",
    },
    // Script grants (script steps phase 1): the operator's host-local
    // consent to run one workspace script at one content digest. No Agent
    // capability, so `classify_agent_tool_grant` makes both tools
    // operator-only and an agent session or plugin callback is refused at
    // the chokepoint; the witness under the global root is likewise only
    // written by the granting command. List and show stay ungoverned
    // reads, grantable to agents.
    GovernedOperation {
        id: "orbit.script.grant",
        surface: OperationSurface::Tool,
        allowed: &[McpCapability::Operator],
        rationale: "granting a script authorizes running its exact bytes unattended on this host, so only an operator may grant one; an agent session never grants the code it can edit",
    },
    GovernedOperation {
        id: "orbit.script.revoke",
        surface: OperationSurface::Tool,
        allowed: &[McpCapability::Operator],
        rationale: "revoking a script grant withdraws an operator's consent to run it, so only an operator may revoke one",
    },
    GovernedOperation {
        id: "script grant",
        surface: OperationSurface::CliCommand,
        allowed: &[McpCapability::Operator],
        rationale: "granting a script authorizes running its exact bytes unattended on this host, so only an operator may grant one; an agent session never grants the code it can edit",
    },
    GovernedOperation {
        id: "script revoke",
        surface: OperationSurface::CliCommand,
        allowed: &[McpCapability::Operator],
        rationale: "revoking a script grant withdraws an operator's consent to run it, so only an operator may revoke one",
    },
    GovernedOperation {
        id: "decision answer",
        surface: OperationSurface::CliCommand,
        allowed: &[McpCapability::Operator],
        rationale: "answering a decision request applies the operator's choice to held work as a human write, so an agent session never answers for a human",
    },
    GovernedOperation {
        id: "decision reverse",
        surface: OperationSurface::CliCommand,
        allowed: &[McpCapability::Operator],
        rationale: "reversing a precedent answer re-opens the request and restores the task status, so only the operator may undo an answer a rule applied",
    },
    GovernedOperation {
        id: "decision rule-auto",
        surface: OperationSurface::CliCommand,
        allowed: &[McpCapability::Operator],
        rationale: "marking a corpus rule auto lets that rule answer a matching request, so only the operator may set it",
    },
    GovernedOperation {
        id: "workspace teardown",
        surface: OperationSurface::CliCommand,
        allowed: &[McpCapability::Operator],
        rationale: "teardown deletes the workspace data root",
    },
    GovernedOperation {
        id: "workspace remove",
        surface: OperationSurface::CliCommand,
        allowed: &[McpCapability::Operator],
        rationale: "removing a workspace deregisters it for every other surface",
    },
    GovernedOperation {
        id: "audit prune",
        surface: OperationSurface::CliCommand,
        allowed: &[McpCapability::Operator],
        rationale: "pruning permanently deletes audit history",
    },
    GovernedOperation {
        id: "gc worktrees",
        surface: OperationSurface::CliCommand,
        allowed: &[McpCapability::Operator, McpCapability::Runner],
        rationale: "collection force-removes worktrees and deletes their branches",
    },
    GovernedOperation {
        id: "gc tmp",
        surface: OperationSurface::CliCommand,
        allowed: &[McpCapability::Operator, McpCapability::Runner],
        rationale: "collection permanently deletes workspace scratch contents",
    },
    GovernedOperation {
        id: "gc deployments",
        surface: OperationSurface::CliCommand,
        allowed: &[McpCapability::Operator],
        rationale: "collection permanently deletes deployment directories and executable and \
                    store backups from the Orbit home",
    },
    GovernedOperation {
        id: "gc audit",
        surface: OperationSurface::CliCommand,
        allowed: &[McpCapability::Operator],
        rationale: "applying audit retention permanently deletes audit history and blobs",
    },
    GovernedOperation {
        id: "gc runs",
        surface: OperationSurface::CliCommand,
        allowed: &[McpCapability::Operator, McpCapability::Runner],
        rationale: "applying run retention permanently drops old runs' pipeline state",
    },
    GovernedOperation {
        id: "gc journal",
        surface: OperationSurface::CliCommand,
        allowed: &[McpCapability::Operator],
        rationale: "applying journal retention permanently deletes applied task commit decisions",
    },
    GovernedOperation {
        id: "routine enable",
        surface: OperationSurface::CliCommand,
        allowed: &[McpCapability::Operator],
        rationale: "enabling a versioned routine definition starts unattended execution",
    },
    GovernedOperation {
        id: "routine disable",
        surface: OperationSurface::CliCommand,
        allowed: &[McpCapability::Operator],
        rationale: "disabling a versioned routine definition changes unattended execution",
    },
    DASHBOARD_ROUTINE_TOGGLE,
    DASHBOARD_ROUTINE_TRIGGER,
    DASHBOARD_JOB_RUN,
    DASHBOARD_LEASE_WRITE,
    DASHBOARD_CLOCK_SERVICE,
    DASHBOARD_CLOCK_CADENCE,
    DASHBOARD_AUTO_TASK_TOGGLE,
    DASHBOARD_AUTO_TASK_SCHEDULE,
    DASHBOARD_AUTO_TASK_MINT,
    DASHBOARD_AUTO_TASK_DELETE,
    DASHBOARD_AUTO_DRAIN_COMPLETE,
    DASHBOARD_AUTO_DRAIN_MODE,
    DASHBOARD_AUTO_DRAIN_APPROVE_PROPOSED,
    DASHBOARD_AUTO_DRAIN_AUTO_FIX,
    DASHBOARD_AUTO_DRAIN_STOP,
    DASHBOARD_HANDOFF_APPROVE,
    DASHBOARD_HANDOFF_REVOKE,
    DASHBOARD_HANDOFF_RETRY,
    DASHBOARD_TASK_REQUEUE,
    DASHBOARD_TASK_SETTLE_MERGED,
    DASHBOARD_TASK_PILOT_ADMISSION,
    DASHBOARD_TASK_POST_DEPLOY,
    DASHBOARD_CLAIM_RECOVER,
    DASHBOARD_CONFIG_SET,
    DASHBOARD_EXPERIMENT_CREATE,
    DASHBOARD_EXPERIMENT_UPDATE,
    DASHBOARD_EXPERIMENT_START,
    DASHBOARD_EXPERIMENT_CONCLUDE,
    DASHBOARD_HOST_EDIT,
    DASHBOARD_HOST_FORWARD,
    DASHBOARD_PLUGIN_ENABLE,
    DASHBOARD_PLUGIN_DISABLE,
    DASHBOARD_SCRIPT_GRANT,
    DASHBOARD_RESIDENT_WRITE,
    PLUGIN_TOOL_READ_ONLY,
    PLUGIN_TOOL_MUTATING,
    PLUGIN_TOOL_AGENT_CALLABLE,
];

/// Look up the governed tool operation for `tool_name`, if any.
///
/// The generic plugin rows are keyed on a manifest's execution kind and
/// consented agent access, not on a tool name, and the
/// chokepoint selects them with [`governed_plugin_tool`](super::governed_plugin_tool).
pub fn governed_tool(tool_name: &str) -> Option<&'static GovernedOperation> {
    GOVERNED_OPERATIONS
        .iter()
        .find(|operation| operation.surface == OperationSurface::Tool && operation.id == tool_name)
}

/// Look up the governed CLI command operation for `<command> <subcommand>`.
pub fn governed_command(command: &str, subcommand: &str) -> Option<&'static GovernedOperation> {
    GOVERNED_OPERATIONS.iter().find(|operation| {
        operation.surface == OperationSurface::CliCommand
            && operation
                .id
                .split_once(' ')
                .is_some_and(|(head, tail)| head == command && tail == subcommand)
    })
}
