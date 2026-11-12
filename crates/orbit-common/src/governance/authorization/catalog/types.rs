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
/// An operation absent from [`GOVERNED_OPERATIONS`](super::GOVERNED_OPERATIONS) has no operation-specific
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
    pub(in crate::governance::authorization) fn satisfied_by(
        &self,
        grants: &BTreeSet<McpCapability>,
    ) -> bool {
        self.allowed
            .iter()
            .any(|capability| grants.contains(capability))
    }

    /// The required capabilities, rendered for a human.
    pub(in crate::governance::authorization) fn allowed_label(&self) -> String {
        self.allowed
            .iter()
            .map(McpCapability::to_string)
            .collect::<Vec<_>>()
            .join(" or ")
    }
}

/// The governed operation an `orbit.task.add` or `orbit.task.update` whose
/// input carries `granted_tools` additionally requires. It names no tool; the
/// tool chokepoint checks it beside the write's own row.
pub const TASK_GRANT_TOOLS_OPERATION_ID: &str = "orbit.task.grant_tools";

/// The resident authority operation. An unidentified local CLI caller reaches
/// it as an agent, so its widening becomes a request the human answers.
pub const RESIDENT_GRANT_OPERATION_ID: &str = "orbit.resident.grant";

/// Versioned routine-definition toggle exposed by the dashboard.
pub const DASHBOARD_ROUTINE_TOGGLE: GovernedOperation = GovernedOperation {
    id: "routine.toggle",
    surface: OperationSurface::Dashboard,
    allowed: &[McpCapability::Operator],
    rationale: "changing a versioned routine definition changes unattended execution",
};

/// Routine trigger replacement from the dashboard.
pub const DASHBOARD_ROUTINE_TRIGGER: GovernedOperation = GovernedOperation {
    id: "routine.set_trigger",
    surface: OperationSurface::Dashboard,
    allowed: &[McpCapability::Operator],
    rationale: "changing a versioned routine trigger changes when unattended execution runs",
};

/// Manual catalog job submission from the dashboard.
pub const DASHBOARD_JOB_RUN: GovernedOperation = GovernedOperation {
    id: "job.run",
    surface: OperationSurface::Dashboard,
    allowed: &[McpCapability::Operator],
    rationale: "starting a job manually can execute workspace work outside its schedule",
};

/// Lease mutations exposed by the dashboard.
pub const DASHBOARD_LEASE_WRITE: GovernedOperation = GovernedOperation {
    id: "lease.write",
    surface: OperationSurface::Dashboard,
    allowed: &[McpCapability::Operator],
    rationale: "changing path leases changes which workspace paths other callers may hold",
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

/// Auto-task schedule replacement from the dashboard.
pub const DASHBOARD_AUTO_TASK_SCHEDULE: GovernedOperation = GovernedOperation {
    id: "auto_task.set_schedule",
    surface: OperationSurface::Dashboard,
    allowed: &[McpCapability::Operator],
    rationale: "changing a versioned auto-task schedule changes when unattended tasks are minted",
};

/// Unconditional on-demand auto-task mint exposed by the dashboard.
pub const DASHBOARD_AUTO_TASK_MINT: GovernedOperation = GovernedOperation {
    id: "auto_task.mint",
    surface: OperationSurface::Dashboard,
    allowed: &[McpCapability::Operator],
    rationale: "manual mint ignores schedule, enabled state, and scheduler dedupe",
};

/// Delete an auto-task definition from the dashboard.
pub const DASHBOARD_AUTO_TASK_DELETE: GovernedOperation = GovernedOperation {
    id: "auto_task.delete",
    surface: OperationSurface::Dashboard,
    allowed: &[McpCapability::Operator],
    rationale: "deleting an auto-task discards its scheduler cursor and delivery consumer \
                state, and a deleted shipped default stays out of later reseeds",
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

/// Opt-in PR blocker repairs for a dashboard-submitted owner auto-drain.
pub const DASHBOARD_AUTO_DRAIN_AUTO_FIX: GovernedOperation = GovernedOperation {
    id: "auto_drain.auto_fix",
    surface: OperationSurface::Dashboard,
    allowed: &[McpCapability::Operator],
    rationale: "repair authority lets the window run agents to fix failed required checks and merge conflicts on its PRs; completion authority remains separate",
};

/// Choose a forecast policy for one dashboard-submitted local drain.
pub const DASHBOARD_AUTO_DRAIN_MODE: GovernedOperation = GovernedOperation {
    id: "auto_drain.auto_concurrency",
    surface: OperationSurface::Dashboard,
    allowed: &[McpCapability::Operator],
    rationale: "choosing a per-drain forecast policy changes unattended admission decisions",
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

/// Retry an existing authorized stopped landing through the owner machinery.
pub const DASHBOARD_HANDOFF_RETRY: GovernedOperation = GovernedOperation {
    id: "handoff.retry",
    surface: OperationSurface::Dashboard,
    allowed: &[McpCapability::Operator],
    rationale: "retrying a stopped authorized handoff is a deliberate owner operator decision",
};

/// Withdraw completion authority for a distributed delivery handoff from the
/// dashboard [ORB-12516].
pub const DASHBOARD_HANDOFF_REVOKE: GovernedOperation = GovernedOperation {
    id: "handoff.revoke",
    surface: OperationSurface::Dashboard,
    allowed: &[McpCapability::Operator],
    rationale: "revocation cancels a pending landing request for work already handed off; only an operator decides that",
};

/// Requeue a blocked task from the dashboard, keeping or discarding its candidate.
pub const DASHBOARD_TASK_REQUEUE: GovernedOperation = GovernedOperation {
    id: "task.requeue",
    surface: OperationSurface::Dashboard,
    allowed: &[McpCapability::Operator],
    rationale: "requeuing a blocked task overrides a recorded escalation and preserves or discards its published candidate",
};

/// Settle a task whose delivery PR merged outside the pipeline, from the dashboard.
pub const DASHBOARD_TASK_SETTLE_MERGED: GovernedOperation = GovernedOperation {
    id: "task.settle_merged",
    surface: OperationSurface::Dashboard,
    allowed: &[McpCapability::Operator],
    rationale: "settling a task from a merged pull request completes it without a run; only an operator decides that",
};

/// Record an operator decision on a task-pilot admission hold from the dashboard.
pub const DASHBOARD_TASK_PILOT_ADMISSION: GovernedOperation = GovernedOperation {
    id: "task.pilot_admission",
    surface: OperationSurface::Dashboard,
    allowed: &[McpCapability::Operator],
    rationale: "clearing or overriding a task-pilot admission hold releases held work on an operator's evidenced decision; only an operator decides that",
};

/// Record an operator measurement of an owed post-deploy criterion from the dashboard.
pub const DASHBOARD_TASK_POST_DEPLOY: GovernedOperation = GovernedOperation {
    id: "task.post_deploy",
    surface: OperationSurface::Dashboard,
    allowed: &[McpCapability::Operator],
    rationale: "recording a deployed-build measurement settles an owed acceptance criterion; only an operator decides that",
};

/// Configuration write from the dashboard's Config tab [ORB-12724].
pub const DASHBOARD_CONFIG_SET: GovernedOperation = GovernedOperation {
    id: "config.set",
    surface: OperationSurface::Dashboard,
    allowed: &[McpCapability::Operator],
    rationale: "config.toml governs sandboxing, crews, and unattended delivery for every surface on this machine",
};

/// Define a draft experiment from the dashboard's Analytics › Experiments page.
pub const DASHBOARD_EXPERIMENT_CREATE: GovernedOperation = GovernedOperation {
    id: "experiment.create",
    surface: OperationSurface::Dashboard,
    allowed: &[McpCapability::Operator],
    rationale: "defining an experiment decides which treatments tasks may be randomized into, so only an operator may do it",
};

/// Edit a draft experiment's definition from the dashboard.
pub const DASHBOARD_EXPERIMENT_UPDATE: GovernedOperation = GovernedOperation {
    id: "experiment.update",
    surface: OperationSurface::Dashboard,
    allowed: &[McpCapability::Operator],
    rationale: "editing an experiment's definition changes the treatments tasks may be randomized into, so only an operator may do it",
};

/// Start a draft experiment from the dashboard.
pub const DASHBOARD_EXPERIMENT_START: GovernedOperation = GovernedOperation {
    id: "experiment.start",
    surface: OperationSurface::Dashboard,
    allowed: &[McpCapability::Operator],
    rationale: "starting an experiment begins randomizing tasks into its treatments, and a live one changes how their runs dispatch, so only an operator may do it",
};

/// Pause a running experiment from the dashboard.
pub const DASHBOARD_EXPERIMENT_PAUSE: GovernedOperation = GovernedOperation {
    id: "experiment.pause",
    surface: OperationSurface::Dashboard,
    allowed: &[McpCapability::Operator],
    rationale: "pausing an experiment stops it enrolling tasks, so only an operator may do it",
};

/// Resume a paused experiment from the dashboard.
pub const DASHBOARD_EXPERIMENT_RESUME: GovernedOperation = GovernedOperation {
    id: "experiment.resume",
    surface: OperationSurface::Dashboard,
    allowed: &[McpCapability::Operator],
    rationale: "resuming an experiment restarts randomizing tasks into its treatments, so only an operator may do it",
};

/// Conclude an experiment from the dashboard, recording the decision and
/// filing a promotion task when an arm is chosen.
pub const DASHBOARD_EXPERIMENT_CONCLUDE: GovernedOperation = GovernedOperation {
    id: "experiment.conclude",
    surface: OperationSurface::Dashboard,
    allowed: &[McpCapability::Operator],
    rationale: "concluding an experiment records the operator's decision, stops assignment and may file a promotion task, so only an operator may do it",
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

/// Grant a workspace script at the digest the operator reviewed, from the
/// dashboard Scripts view (script steps §2, route 2).
pub const DASHBOARD_SCRIPT_GRANT: GovernedOperation = GovernedOperation {
    id: "script.grant",
    surface: OperationSurface::Dashboard,
    allowed: &[McpCapability::Operator],
    rationale: "a script grant lets unattended job steps run those exact bytes on this host",
};

/// Set up and manage resident mandates from the dashboard's Automation ›
/// Residents view: create a mandate, narrow or widen its authority, answer
/// its `resident_authority` requests, relay to it and control its firing.
/// A dashboard widening raises the grant request and answers it as the
/// operator in one action; it never applies as a native-terminal grant.
pub const DASHBOARD_RESIDENT_WRITE: GovernedOperation = GovernedOperation {
    id: "resident.write",
    surface: OperationSurface::Dashboard,
    allowed: &[McpCapability::Operator],
    rationale: "resident mandates decide what runs unattended and with which rights, so only an operator dashboard session may set them up, widen them or steer them",
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
/// registry through generic rows rather than one row per tool
/// (design `docs/design/plugins/1_scope.md` §4.1). The manifest chooses the
/// execution kind and requests agent access; recorded operator consent is
/// required before that declaration can activate.
///
/// Its `id` is not a tool name and is never matched by [`governed_tool`](super::governed_tool): the
/// tool chokepoint resolves a plugin call to one of these rows through
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
/// Activity allowlists never confer this row's capability. A consented
/// `agent_callable` declaration selects a separate row instead.
pub const PLUGIN_TOOL_MUTATING: GovernedOperation = GovernedOperation {
    id: "plugin.tool.mutating",
    surface: OperationSurface::Tool,
    allowed: &[McpCapability::Operator, McpCapability::Runner],
    rationale: "a mutating plugin tool requires operator or runner capability unless its agent_callable declaration has operator consent; an activity allowlist alone confers no capability",
};

/// An append-only or proposal-only mutating plugin tool whose manifest
/// requests agent access and whose `agent_tools` grant the host verified.
pub const PLUGIN_TOOL_AGENT_CALLABLE: GovernedOperation = GovernedOperation {
    id: "plugin.tool.agent_callable",
    surface: OperationSurface::Tool,
    allowed: &[
        McpCapability::Agent,
        McpCapability::Operator,
        McpCapability::Runner,
    ],
    rationale: "an agent_callable mutating plugin tool has explicit operator consent through the agent_tools grant; activity allowlists, denials and sandbox restrictions still apply",
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
/// A mutating tool selects [`PLUGIN_TOOL_AGENT_CALLABLE`] only with the
/// consented declaration; otherwise it selects [`PLUGIN_TOOL_MUTATING`].
/// A read-only tool gets [`PLUGIN_TOOL_READ_ONLY`].
pub fn governed_plugin_tool(mutating: bool, agent_callable: bool) -> &'static GovernedOperation {
    if mutating && agent_callable {
        &PLUGIN_TOOL_AGENT_CALLABLE
    } else if mutating {
        &PLUGIN_TOOL_MUTATING
    } else {
        &PLUGIN_TOOL_READ_ONLY
    }
}
