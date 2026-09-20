//! Operation authorization shared by Orbit's execution surfaces.
//!
//! This module declares which operations are governed, which capabilities may
//! perform them, and the decision function every enforcing surface calls. Every
//! surface — CLI, MCP, dashboard, managed run — resolves the same capability
//! vocabulary here; they differ only in which signals may contribute to it,
//! which is [`CapabilityResolution`].
//!
//! # What this is, and what it is not
//!
//! This is an **accident guard, not a security boundary**. Every agent on a
//! development box runs as the same OS user and can bypass Orbit entirely with
//! `git`, `rm`, or a direct write to the data root. Nothing here changes that,
//! and no design that pretends otherwise — a password, a token, a keychain —
//! belongs here: it would buy no protection while inviting the surrounding
//! rails to be relaxed on the strength of a boundary that does not exist.
//!
//! The goal is narrower and achievable: unintended destruction fails loudly
//! and leaves a record.
//!
//! # Placement is not permission
//!
//! Orbit has two independent axes, and this module owns exactly one of them.
//!
//! *Placement* is [`crate::governance::operation::OperationSpec::mcp_scope`] and its
//! registry counterparts (`register_mcp`, `register`, `register_inactive`):
//! which surfaces *list* a tool. It is an audience decision — what an agent
//! reading `tools/list` is pointed at — and it authorizes nothing.
//! `tools/list` does no capability filtering, and the tool registry's
//! `execute` never consults availability, so a tool registered active but
//! unadvertised is still reachable through `orbit tool run`.
//!
//! `register_inactive` is the one placement that also removes reach: CLI
//! dispatch applies `ensure_tool_agent_facing` too, so an inactive tool needs
//! a command that reaches the runtime directly — the way
//! `orbit task locks release` does — and registering one without that command
//! leaves it callable from nowhere [ORB-12581].
//!
//! *Permission* for exceptional operations is [`GOVERNED_OPERATIONS`], resolved
//! by [`authorize`] at one chokepoint per surface. A session that arrived over
//! SSH is decided the same way as a local one: the destination honors the
//! authority in the argv it was started with, because an SSH login to a machine
//! is ownership of it [ORB-12564]. The registry remains the only
//! operation-specific
//! authorization statement Orbit makes, and its answer is surface-independent:
//! the same answer for an MCP call, a CLI `tool run`, the dashboard, and the
//! deterministic dispatcher.
//!
//! The two therefore need not agree, and deliberately do not. A tool may be
//! advertised and governed (the operator MCP surface: a session served by
//! `orbit mcp serve --operator` sees and performs it, an ordinary agent session
//! sees and is refused), or unadvertised and ungoverned (`orbit friction show`
//! — kept off the agent MCP surface because `list` already returns what an
//! agent needs, but freely readable from any CLI).
//! What must never happen is for a pairing to change by accident. The
//! guardrail that pins every governed tool's placement is
//! `crates/orbit-tools/src/builtin/orbit/tests/authorization.rs`, which lives
//! in the lowest crate that can see both axes at once [ORB-10478].
//!
//! # Layering
//!
//! The kernel lives in the leaf crate for the same reason the operations-as-data
//! registry does (see [`crate::governance::operation`]): every consumer surface must be able
//! to read it without acquiring a new dependency edge. It holds no runtime
//! handle, no transport types, and no store — surfaces feed it a
//! [`CallerEnvelope`] and it answers.

use std::collections::BTreeSet;
use std::fmt::{Display, Formatter};

use orbit_types::tool::{McpCapability, ToolSessionContext};

/// Environment variable that grants [`McpCapability::Operator`] to a caller the
/// envelope would otherwise leave unprivileged.
///
/// This is the escape hatch, and it is deliberately trivial to set: it is not
/// defending against a determined caller, it is making a deliberate act look
/// deliberate. Every use is logged and audited with
/// [`CallerProvenance::OperatorOverride`] so an override is visible in the
/// trail rather than indistinguishable from an ordinary operator session.
pub const OPERATOR_OVERRIDE_ENV: &str = "ORBIT_OPERATOR";

/// Whether [`OPERATOR_OVERRIDE_ENV`] is set to a truthy value in this
/// process's environment.
///
/// Exposed so a caller outside this module — actor-identity resolution, in
/// particular — can tell "an operator deliberately raised this" apart from
/// "nothing identified the caller" without re-deriving the truthy spellings
/// this module already owns.
pub fn operator_override_active() -> bool {
    env_truthy(OPERATOR_OVERRIDE_ENV)
}

/// Process-envelope variables that declare the caller to be an agent.
const AGENT_ENVELOPE_ENV: &[&str] = &["ORBIT_AGENT_NAME", "ORBIT_AGENT_MODEL"];

/// Whether this process's environment declares it to be an agent or a step of
/// a managed run.
///
/// Exposed so a caller outside this module can refuse to *propagate* operator
/// authority it may itself hold — the federated MCP client asks this before
/// composing a destination's argv, so a server an agent launched cannot hand
/// operator authority onward to another machine [ORB-12564]. It is the same
/// observation [`CallerEnvelope::from_process_env`] makes, named once so the
/// two cannot disagree about what an agent looks like.
pub fn agent_context_declared() -> bool {
    agent_declared_in_env()
}

/// Set to `agent` by the activity runner for agent-backed steps.
const ACTOR_KIND_ENV: &str = "ORBIT_TASK_ACTOR_KIND";

/// Set by the engine for any process it spawns as part of a managed run.
const MANAGED_RUN_ENV: &str = "ORBIT_MANAGED_RUN_CONTEXT";

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
    fn satisfied_by(&self, grants: &BTreeSet<McpCapability>) -> bool {
        self.allowed
            .iter()
            .any(|capability| grants.contains(capability))
    }

    /// The required capabilities, rendered for a human.
    fn allowed_label(&self) -> String {
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

/// Stop new admissions under an operation-mode grant from the dashboard
/// [ORB-11332].
pub const DASHBOARD_OPERATION_STOP: GovernedOperation = GovernedOperation {
    id: "operation.stop",
    surface: OperationSurface::Dashboard,
    allowed: &[McpCapability::Operator],
    rationale: "stopping a grant ends scoped automation for the workspace; only an operator decides that",
};

/// Hard-revoke an operation-mode grant from the dashboard [ORB-11332].
pub const DASHBOARD_OPERATION_REVOKE: GovernedOperation = GovernedOperation {
    id: "operation.revoke",
    surface: OperationSurface::Dashboard,
    allowed: &[McpCapability::Operator],
    rationale: "revocation withdraws completion authority from admitted work; only an operator decides that",
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
        id: "orbit.workflow.run.workers",
        surface: OperationSurface::Tool,
        allowed: &[McpCapability::Operator],
        rationale: "retuning a live drain's worker ceiling changes how much work the workspace starts",
    },
    GovernedOperation {
        id: "orbit.operation.enable",
        surface: OperationSurface::Tool,
        allowed: &[McpCapability::Operator],
        rationale: "enabling a grant authorizes automatic preparation, promotion, and possibly completion for a task set without asking again",
    },
    GovernedOperation {
        id: "orbit.operation.stop",
        surface: OperationSurface::Tool,
        allowed: &[McpCapability::Operator],
        rationale: "stopping a grant ends scoped automation for the workspace",
    },
    GovernedOperation {
        id: "orbit.operation.revoke",
        surface: OperationSurface::Tool,
        allowed: &[McpCapability::Operator],
        rationale: "revocation withdraws completion authority from admitted work",
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
        id: "orbit.semantic.uninstall",
        surface: OperationSurface::Tool,
        allowed: &[McpCapability::Operator],
        rationale: "uninstalling tears down the local semantic index and companion",
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
        id: "semantic uninstall",
        surface: OperationSurface::CliCommand,
        allowed: &[McpCapability::Operator],
        rationale: "uninstalling tears down the local semantic index and companion",
    },
    GovernedOperation {
        id: "gc worktrees",
        surface: OperationSurface::CliCommand,
        allowed: &[McpCapability::Operator, McpCapability::Runner],
        rationale: "collection force-removes worktrees and deletes their branches",
    },
    DASHBOARD_ROUTINE_TOGGLE,
    DASHBOARD_CLOCK_SERVICE,
    DASHBOARD_CLOCK_CADENCE,
    DASHBOARD_AUTO_TASK_TOGGLE,
    DASHBOARD_AUTO_TASK_MINT,
    DASHBOARD_AUTO_DRAIN_COMPLETE,
    DASHBOARD_OPERATION_STOP,
    DASHBOARD_OPERATION_REVOKE,
];

/// Look up the governed tool operation for `tool_name`, if any.
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

/// Where a caller's capabilities came from.
///
/// Provenance is recorded rather than discarded because the audit trail needs
/// to distinguish "an operator did this" from "something set the override
/// variable", and because a denial message is only actionable if it can say
/// what Orbit thought the caller was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallerProvenance {
    /// A trusted transport or the run's own dispatcher asserted the grants.
    Session,
    /// [`OPERATOR_OVERRIDE_ENV`] was set. Always logged, never silent.
    OperatorOverride,
    /// The process environment declares an agent envelope.
    AgentEnvelope,
    /// Standard input and error are both terminals — a person is present.
    InteractiveTerminal,
    /// Nothing identified the caller. Grants nothing.
    Unknown,
}

impl Display for CallerProvenance {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Session => "session",
            Self::OperatorOverride => "operator-override",
            Self::AgentEnvelope => "agent-envelope",
            Self::InteractiveTerminal => "interactive-terminal",
            Self::Unknown => "unknown",
        })
    }
}

/// Which signals a surface allows to contribute to a caller's capabilities.
///
/// This is not a second authorization rule — [`authorize`] is unchanged by it.
/// It records what the surface was willing to look at, which is exactly what a
/// refused caller needs to know to act: the escape hatch works on one of these
/// surfaces and is inert on the other.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CapabilityResolution {
    /// The hosting process's environment counts, so [`OPERATOR_OVERRIDE_ENV`]
    /// and the agent envelope contribute. This is every non-MCP surface.
    #[default]
    ProcessEnvelope,
    /// Only grants asserted by the trusted session count. The MCP surface
    /// resolves this way so an agent session cannot inherit operator authority
    /// from whatever process happens to host its server.
    SessionOnly,
}

/// The raw signals a chokepoint observed about its caller.
///
/// Kept as plain data so the resolution rules are testable without mutating
/// process state, which no test can do safely in a threaded runner.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CallerEnvelope {
    /// Which signals this surface honors.
    pub resolution: CapabilityResolution,
    /// Grants asserted by a trusted seam: a validated MCP session, or the run
    /// dispatcher stamping its own sanction onto a tool context.
    pub session_capabilities: BTreeSet<McpCapability>,
    /// [`OPERATOR_OVERRIDE_ENV`] is set to a truthy value.
    pub operator_override: bool,
    /// The process environment declares an agent or a managed run.
    pub agent_declared: bool,
    /// Standard input and error are both terminals.
    pub interactive_terminal: bool,
    /// Caller label an SSH-originated MCP session forwarded
    /// (`--remote-caller-machine-id`) [ORB-12564].
    ///
    /// Attribution, never a grant: it rides beside
    /// [`Self::session_capabilities`] so a denial and its audit row can name
    /// which machine reached in, and it contributes nothing to the decision.
    pub remote_caller_machine_id: Option<String>,
}

impl CallerEnvelope {
    /// Observe the current process, layered under `session`'s asserted grants.
    pub fn from_process_env(session: &ToolSessionContext) -> Self {
        Self {
            resolution: CapabilityResolution::ProcessEnvelope,
            session_capabilities: session.effective_capabilities.clone(),
            operator_override: env_truthy(OPERATOR_OVERRIDE_ENV),
            agent_declared: agent_declared_in_env(),
            interactive_terminal: interactive_terminal(),
            remote_caller_machine_id: session.remote_caller_machine_id().map(ToOwned::to_owned),
        }
    }

    /// Read `session`'s grants and nothing else.
    ///
    /// The MCP chokepoint builds its envelope here. The server process decides
    /// once, at startup, what authority it serves sessions with; discarding the
    /// live process environment is what keeps that decision from being reopened
    /// per call by whatever the hosting agent exported.
    pub fn mcp_session(session: &ToolSessionContext) -> Self {
        Self {
            resolution: CapabilityResolution::SessionOnly,
            session_capabilities: session.effective_capabilities.clone(),
            remote_caller_machine_id: session.remote_caller_machine_id().map(ToOwned::to_owned),
            ..Self::default()
        }
    }
}

/// A caller's resolved capabilities and how Orbit arrived at them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallerCapabilities {
    grants: BTreeSet<McpCapability>,
    provenance: CallerProvenance,
    resolution: CapabilityResolution,
    remote_caller_machine_id: Option<String>,
}

impl CallerCapabilities {
    /// Resolve an envelope into a capability set.
    ///
    /// Precedence, highest first:
    ///
    /// 1. **Session grants.** A validated MCP session or a run-stamped tool
    ///    context already carries an authorization decision made by a trusted
    ///    seam; re-deriving it from ambient process state would be strictly
    ///    worse information.
    /// 2. **Operator override.** Explicit, loud, audited.
    /// 3. **Agent envelope.** An agent gets [`McpCapability::Agent`] and
    ///    nothing more, whether or not it happens to have a terminal.
    /// 4. **Interactive terminal.** A person at a TTY is the one caller Orbit
    ///    can positively identify as an operator without a credential.
    /// 5. **Nothing.** An unidentified caller gets an empty set, and every
    ///    governed operation therefore denies. Ambiguity fails closed.
    pub fn resolve(envelope: &CallerEnvelope) -> Self {
        let (grants, provenance) = Self::resolve_grants(envelope);
        Self {
            grants,
            provenance,
            resolution: envelope.resolution,
            remote_caller_machine_id: envelope.remote_caller_machine_id.clone(),
        }
    }

    fn resolve_grants(envelope: &CallerEnvelope) -> (BTreeSet<McpCapability>, CallerProvenance) {
        if !envelope.session_capabilities.is_empty() {
            return (
                envelope.session_capabilities.clone(),
                CallerProvenance::Session,
            );
        }
        if envelope.operator_override {
            return (
                BTreeSet::from([McpCapability::Operator]),
                CallerProvenance::OperatorOverride,
            );
        }
        if envelope.agent_declared {
            return (
                BTreeSet::from([McpCapability::Agent]),
                CallerProvenance::AgentEnvelope,
            );
        }
        if envelope.interactive_terminal {
            return (
                BTreeSet::from([McpCapability::Operator]),
                CallerProvenance::InteractiveTerminal,
            );
        }
        (BTreeSet::new(), CallerProvenance::Unknown)
    }

    /// The resolved grants.
    pub fn grants(&self) -> &BTreeSet<McpCapability> {
        &self.grants
    }

    /// How the grants were derived.
    pub fn provenance(&self) -> CallerProvenance {
        self.provenance
    }

    /// Whether this caller reached authorization through the escape hatch.
    pub fn is_override(&self) -> bool {
        self.provenance == CallerProvenance::OperatorOverride
    }

    /// The caller label an SSH-originated session forwarded, if any.
    ///
    /// Attribution for the audit trail; it grants nothing and is only as
    /// strong as the SSH login behind it.
    pub fn remote_caller_machine_id(&self) -> Option<&str> {
        self.remote_caller_machine_id.as_deref()
    }

    /// The grants, rendered for a human.
    fn grants_label(&self) -> String {
        capabilities_label(&self.grants)
    }
}

/// A capability set, rendered for a human. Empty reads as `none` rather than
/// as an empty bracket a reader would have to interpret.
fn capabilities_label(capabilities: &BTreeSet<McpCapability>) -> String {
    if capabilities.is_empty() {
        return "none".to_string();
    }
    capabilities
        .iter()
        .map(McpCapability::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// A refused governed operation.
///
/// Carries the structured facts a caller needs to act — what was refused, what
/// it required, what the caller actually held, and the one documented way to
/// proceed — so no surface has to reconstruct the message from a string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizationDenial {
    /// The refused operation.
    pub operation: &'static GovernedOperation,
    /// What the caller held at the moment of refusal.
    pub granted: String,
    /// How those grants were derived.
    pub provenance: CallerProvenance,
    /// Which signals the refusing surface honored, and therefore which remedy
    /// the caller actually has.
    pub resolution: CapabilityResolution,
    /// Caller label an SSH-originated session forwarded, for attribution in
    /// the denial's log and audit row.
    pub remote_caller_machine_id: Option<String>,
}

impl AuthorizationDenial {
    /// The one documented way for this caller, on this surface, to proceed.
    ///
    /// The remedy is surface-specific because [`OPERATOR_OVERRIDE_ENV`] is
    /// deliberately ignored under [`CapabilityResolution::SessionOnly`];
    /// advising it there would send an operator in a circle.
    ///
    /// A session that arrived over SSH gets the same session-only remedy as a
    /// local one, and it is reachable: the destination honors the authority in
    /// the argv the caller's federated server composed, so serving the
    /// *calling* side with `--operator` raises it [ORB-12564].
    fn remedy(&self) -> String {
        match self.resolution {
            CapabilityResolution::ProcessEnvelope => format!(
                "If this is a deliberate operator action, re-run it with {OPERATOR_OVERRIDE_ENV}=1 \
                 set — the override is recorded in the audit trail."
            ),
            CapabilityResolution::SessionOnly => format!(
                "This MCP session's capabilities come from the server process it was served by, \
                 and {OPERATOR_OVERRIDE_ENV} in that process's environment is deliberately \
                 ignored. To perform this as an operator, serve the session from a server \
                 started as `orbit mcp serve --operator` — including a federated or remote-proxy \
                 server on the calling machine, whose `--operator` propagates into the argv every \
                 SSH destination is started with — or run the operation from the CLI."
            ),
        }
    }
}

impl Display for AuthorizationDenial {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "operation '{operation}' requires the `{required}` capability ({rationale}); \
             this caller was resolved as `{provenance}` holding [{granted}]. {remedy}",
            operation = self.operation.id,
            required = self.operation.allowed_label(),
            rationale = self.operation.rationale,
            provenance = self.provenance,
            granted = self.granted,
            remedy = self.remedy(),
        )
    }
}

/// Decide whether `caller` may perform `operation`.
///
/// This is the single decision function. Both chokepoints — the tool path in
/// `orbit-core` and the CLI command path in `orbit-cli` — call it and neither
/// reimplements any part of the rule.
pub fn authorize(
    operation: &'static GovernedOperation,
    caller: &CallerCapabilities,
) -> Result<(), AuthorizationDenial> {
    if operation.satisfied_by(&caller.grants) {
        return Ok(());
    }
    Err(AuthorizationDenial {
        operation,
        granted: caller.grants_label(),
        provenance: caller.provenance,
        resolution: caller.resolution,
        remote_caller_machine_id: caller.remote_caller_machine_id.clone(),
    })
}

/// Whether an environment variable is set to a truthy value.
///
/// Accepts the same spellings as the engine's managed-run flag so a caller does
/// not have to remember which Orbit variable takes which vocabulary.
fn env_truthy(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

fn env_present(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| !value.trim().is_empty())
}

fn agent_declared_in_env() -> bool {
    if AGENT_ENVELOPE_ENV.iter().copied().any(env_present) {
        return true;
    }
    if env_truthy(MANAGED_RUN_ENV) {
        return true;
    }
    std::env::var(ACTOR_KIND_ENV).is_ok_and(|value| value.trim().eq_ignore_ascii_case("agent"))
}

/// Whether a person is plausibly present at this process.
///
/// Both streams are required. `stdin` alone is true for a piped-output shell
/// pipeline, and `stderr` alone is true for a subprocess whose stderr was
/// inherited from a terminal — neither on its own distinguishes a person from
/// an automated caller that happened to inherit one handle.
fn interactive_terminal() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}
