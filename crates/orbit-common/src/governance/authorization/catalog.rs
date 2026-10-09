use std::collections::BTreeSet;

use orbit_types::tool::McpCapability;

/// Which entry surface reaches a governed operation.
///
/// Tool operations are keyed by canonical tool name and enforced at the tool
/// chokepoint; command operations are keyed by `"<command> <subcommand>"` and
/// enforced at the CLI dispatch chokepoint. The split exists because Orbit has
/// destructive operations that are not tools (`workspace teardown` deletes a
/// data root directly), not because there are two authorization models.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum OperationSurface {
    /// A registry-backed tool, reached through `OrbitRuntime::run_tool*`.
    Tool,
    /// A CLI command that performs its destruction without a tool.
    CliCommand,
    /// A typed state-changing dashboard action.
    Dashboard,
}

/// One operation whose performance requires a capability.
///
/// An operation absent from [`GOVERNED_OPERATIONS`] has no operation-specific
/// capability check. Governance is opt-in per operation on purpose: the
/// registry is meant to name the operations whose accidental invocation
/// actually destroys something, not to become a second copy of the tool
/// registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GovernedOperation {
    /// Canonical tool name, or `"<command> <subcommand>"` for a CLI command.
    pub id: &'static str,
    /// Which chokepoint enforces this operation.
    pub surface: OperationSurface,
    /// Capabilities that may perform it. A caller holding **any** of these is
    /// authorized.
    pub allowed: &'static [McpCapability],
    /// Why this operation is governed. Surfaced in the denial message, so it
    /// is written for the person who just got refused.
    pub rationale: &'static str,
}

impl GovernedOperation {
    /// Whether any of `grants` satisfies this operation.
    pub(super) fn satisfied_by(&self, grants: &BTreeSet<McpCapability>) -> bool {
        self.allowed
            .iter()
            .any(|capability| grants.contains(capability))
    }

    /// The required capabilities, rendered for a human.
    pub(super) fn allowed_label(&self) -> String {
        self.allowed
            .iter()
            .map(McpCapability::to_string)
            .collect::<Vec<_>>()
            .join(" or ")
    }
}

/// Versioned routine-definition toggle exposed by the dashboard.
pub const DASHBOARD_ROUTINE_TOGGLE: GovernedOperation = GovernedOperation {
    id: "routine.toggle",
    surface: OperationSurface::Dashboard,
    allowed: &[McpCapability::Operator],
    rationale: "changing a versioned routine definition changes unattended execution",
};

/// Manual catalog job submission from the dashboard.
pub const DASHBOARD_JOB_RUN: GovernedOperation = GovernedOperation {
    id: "job.run",
    surface: OperationSurface::Dashboard,
    allowed: &[McpCapability::Operator],
    rationale: "starting a job manually can execute workspace work outside its schedule",
};

/// Native sweep-clock start/stop action exposed by the dashboard.
pub const DASHBOARD_CLOCK_SERVICE: GovernedOperation = GovernedOperation {
    id: "clock.service",
    surface: OperationSurface::Dashboard,
    allowed: &[McpCapability::Operator],
    rationale: "starting or stopping the host sweep clock changes unattended execution",
};

/// Native sweep-clock cadence action exposed by the dashboard.
pub const DASHBOARD_CLOCK_CADENCE: GovernedOperation = GovernedOperation {
    id: "clock.cadence",
    surface: OperationSurface::Dashboard,
    allowed: &[McpCapability::Operator],
    rationale: "changing the host sweep cadence reloads the native clock service",
};

/// Versioned auto-task definition toggle exposed by the dashboard.
pub const DASHBOARD_AUTO_TASK_TOGGLE: GovernedOperation = GovernedOperation {
    id: "auto_task.toggle",
    surface: OperationSurface::Dashboard,
    allowed: &[McpCapability::Operator],
    rationale: "changing a versioned auto-task definition changes unattended task minting",
};

/// Unconditional on-demand auto-task mint exposed by the dashboard.
pub const DASHBOARD_AUTO_TASK_MINT: GovernedOperation = GovernedOperation {
    id: "auto_task.mint",
    surface: OperationSurface::Dashboard,
    allowed: &[McpCapability::Operator],
    rationale: "manual mint ignores schedule, enabled state, and scheduler dedupe",
};

/// Opt-in `CompletionPolicy::Done` for a dashboard-submitted bounded
/// auto-drain window (the `--complete` equivalent of `orbit run auto`).
pub const DASHBOARD_AUTO_DRAIN_COMPLETE: GovernedOperation = GovernedOperation {
    id: "auto_drain.complete",
    surface: OperationSurface::Dashboard,
    allowed: &[McpCapability::Operator],
    rationale: "opting into automatic completion authorizes review -> done for every task the drain window ships, not only the ones visible at submission",
};

/// Opt-in `--approve-proposed` for a dashboard-submitted bounded auto-drain
/// window (the `--approve-proposed` equivalent of `orbit run auto`).
pub const DASHBOARD_AUTO_DRAIN_APPROVE_PROPOSED: GovernedOperation = GovernedOperation {
    id: "auto_drain.approve_proposed",
    surface: OperationSurface::Dashboard,
    allowed: &[McpCapability::Operator],
    rationale: "opting in lets every pass of the window approve qualifying proposed tasks into the backlog, including ones filed after submission",
};

/// Stop new admissions for the workspace's live auto-drain window from the
/// dashboard (the `--stop` equivalent of `orbit run auto`) [ORB-12728].
pub const DASHBOARD_AUTO_DRAIN_STOP: GovernedOperation = GovernedOperation {
    id: "auto_drain.stop",
    surface: OperationSurface::Dashboard,
    allowed: &[McpCapability::Operator],
    rationale: "stopping admissions ends the workspace's unattended delivery window early; only an operator decides that",
};

/// Record completion authority for a distributed delivery handoff from the
/// dashboard [ORB-12516].
pub const DASHBOARD_HANDOFF_APPROVE: GovernedOperation = GovernedOperation {
    id: "handoff.approve",
    surface: OperationSurface::Dashboard,
    allowed: &[McpCapability::Operator],
    rationale: "approving a handoff authorizes the owner to merge that exact candidate; a follower's agent access never decides that",
};

/// Withdraw completion authority for a distributed delivery handoff from the
/// dashboard [ORB-12516].
pub const DASHBOARD_HANDOFF_REVOKE: GovernedOperation = GovernedOperation {
    id: "handoff.revoke",
    surface: OperationSurface::Dashboard,
    allowed: &[McpCapability::Operator],
    rationale: "revocation cancels a pending landing request for work already handed off; only an operator decides that",
};

/// Configuration write from the dashboard's Config tab [ORB-12724].
pub const DASHBOARD_CONFIG_SET: GovernedOperation = GovernedOperation {
    id: "config.set",
    surface: OperationSurface::Dashboard,
    allowed: &[McpCapability::Operator],
    rationale: "config.toml governs sandboxing, crews, and unattended delivery for every surface on this machine",
};

/// Add, rename or remove a host-file entry from the dashboard's Settings ›
/// Hosts view [ORB-14451].
pub const DASHBOARD_HOST_EDIT: GovernedOperation = GovernedOperation {
    id: "host.edit",
    surface: OperationSurface::Dashboard,
    allowed: &[McpCapability::Operator],
    rationale: "the host file decides where federated MCP, pull drains and routed task calls go for every surface on this machine",
};

/// Forward a write (POST, PUT, PATCH or DELETE) from the dashboard to a
/// registered host's own dashboard through `/api/on/<host>/…` [ORB-14679].
pub const DASHBOARD_HOST_FORWARD: GovernedOperation = GovernedOperation {
    id: "host.forward",
    surface: OperationSurface::Dashboard,
    allowed: &[McpCapability::Operator],
    rationale: "a forwarded write changes another registered host's state through this host's SSH identity",
};

/// Enable a plugin from the dashboard without changing recorded consent.
pub const DASHBOARD_PLUGIN_ENABLE: GovernedOperation = GovernedOperation {
    id: "plugin.enable",
    surface: OperationSurface::Dashboard,
    allowed: &[McpCapability::Operator],
    rationale: "enabling a plugin exposes its tools and scheduled contributions",
};

/// Disable a plugin from the dashboard at host or workspace scope.
pub const DASHBOARD_PLUGIN_DISABLE: GovernedOperation = GovernedOperation {
    id: "plugin.disable",
    surface: OperationSurface::Dashboard,
    allowed: &[McpCapability::Operator],
    rationale: "disabling a plugin removes its tools and scheduled contributions",
};

/// Deliberate recovery of an execution claim from the dashboard [ORB-12516].
pub const DASHBOARD_CLAIM_RECOVER: GovernedOperation = GovernedOperation {
    id: "claim.recover",
    surface: OperationSurface::Dashboard,
    allowed: &[McpCapability::Operator],
    rationale: "recovery fences a live attempt on another host and moves its task; there is no heartbeat, so a human decides the attempt is over",
};

/// Generic row for a `read_only` plugin tool.
///
/// Plugin tools are not enumerable at compile time, so they enter the
/// registry through one row per execution kind rather than one row per tool
/// (design `docs/design/plugins/1_scope.md` §4.1). The manifest chooses the
/// execution kind; it never chooses who may call.
///
/// Its `id` is not a tool name and is never matched by [`governed_tool`]: the
/// tool chokepoint resolves a plugin call to one of these two rows through
/// [`governed_plugin_tool`].
pub const PLUGIN_TOOL_READ_ONLY: GovernedOperation = GovernedOperation {
    id: "plugin.tool.read_only",
    surface: OperationSurface::Tool,
    allowed: &[
        McpCapability::Agent,
        McpCapability::Operator,
        McpCapability::Runner,
    ],
    rationale: "a read-only plugin tool observes without changing anything, so every caller this process can name may run it",
};

/// Generic row for a `mutating` plugin tool.
///
/// An agent reaches one only through the ordinary activity/`required_tools`
/// allowlist, which is applied separately at the same chokepoint: this row is
/// the capability floor, not the allowlist.
pub const PLUGIN_TOOL_MUTATING: GovernedOperation = GovernedOperation {
    id: "plugin.tool.mutating",
    surface: OperationSurface::Tool,
    allowed: &[McpCapability::Operator, McpCapability::Runner],
    rationale: "a mutating plugin tool runs an installed backend that changes state, so it is an                 operator or sanctioned-run operation unless the task's `required_tools` or the                 activity allowlist names it",
};

/// Guarded task edits require an identified caller, as distinct from the
/// operator-only completion and shipment operations.
pub const DESKTOP_TASK_EDIT: GovernedOperation = GovernedOperation {
    id: "orbit.desktop.task.edit",
    surface: OperationSurface::Tool,
    allowed: &[
        McpCapability::Agent,
        McpCapability::Operator,
        McpCapability::Runner,
    ],
    rationale: "guarded task edits require an identified caller; request fields do not grant authority",
};

/// Desktop review completion is a governed suboperation of guarded task writes.
/// A UI action or attributable agent label cannot grant this capability.
pub const DESKTOP_TASK_COMPLETE: GovernedOperation = GovernedOperation {
    id: "orbit.desktop.task.complete",
    surface: OperationSurface::Tool,
    allowed: &[McpCapability::Operator],
    rationale: "desktop acceptance may complete reviewed work only through an existing operator-authorized session; opening the UI grants no authority",
};

/// The generic row a plugin tool of this execution kind is authorized by.
///
/// `mutating: true` selects [`PLUGIN_TOOL_MUTATING`]; a read-only tool gets
/// [`PLUGIN_TOOL_READ_ONLY`].
pub fn governed_plugin_tool(mutating: bool) -> &'static GovernedOperation {
    if mutating {
        &PLUGIN_TOOL_MUTATING
    } else {
        &PLUGIN_TOOL_READ_ONLY
    }
}

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
        id: "orbit.task.reconcile_review",
        surface: OperationSurface::Tool,
        allowed: &[McpCapability::Operator],
        rationale: "reconciliation produces the owner-held review and validation evidence that lets a changed merged head complete",
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
        id: "orbit.workflow.ship",
        surface: OperationSurface::Tool,
        allowed: &[McpCapability::Operator],
        rationale: "dispatching a ship workflow creates managed runs and worktrees",
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
        id: "orbit.agent.invoke",
        surface: OperationSurface::Tool,
        // Deliberately not `Runner`. Every other run-reachable operation lists
        // it so a sanctioned run can perform its own work; this one must not,
        // because the whole point of the mode is to leave the sandbox a managed
        // run exists to stay inside. A run that could admit itself would be a
        // sandbox escape wearing an authorization.
        allowed: &[McpCapability::Operator],
        rationale: "an agent invocation runs a provider subprocess on the host outside the executor sandbox, so it requires a local operator or an explicitly scoped remote operator grant",
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
    DASHBOARD_ROUTINE_TOGGLE,
    DASHBOARD_JOB_RUN,
    DASHBOARD_CLOCK_SERVICE,
    DASHBOARD_CLOCK_CADENCE,
    DASHBOARD_AUTO_TASK_TOGGLE,
    DASHBOARD_AUTO_TASK_MINT,
    DASHBOARD_AUTO_DRAIN_COMPLETE,
    DASHBOARD_AUTO_DRAIN_APPROVE_PROPOSED,
    DASHBOARD_AUTO_DRAIN_STOP,
    DASHBOARD_HANDOFF_APPROVE,
    DASHBOARD_HANDOFF_REVOKE,
    DASHBOARD_CLAIM_RECOVER,
    DASHBOARD_CONFIG_SET,
    DASHBOARD_HOST_EDIT,
    DASHBOARD_HOST_FORWARD,
    DASHBOARD_PLUGIN_ENABLE,
    DASHBOARD_PLUGIN_DISABLE,
    PLUGIN_TOOL_READ_ONLY,
    PLUGIN_TOOL_MUTATING,
];

/// Look up the governed tool operation for `tool_name`, if any.
///
/// The two generic plugin rows are deliberately unreachable here: they are
/// keyed on a manifest's execution kind, not on a tool name, and the
/// chokepoint selects them with [`governed_plugin_tool`].
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

/// Look up a governed dashboard operation by its stable typed action id.
pub fn governed_dashboard(id: &str) -> Option<&'static GovernedOperation> {
    GOVERNED_OPERATIONS
        .iter()
        .find(|operation| operation.surface == OperationSurface::Dashboard && operation.id == id)
}
