use std::collections::BTreeSet;
use std::fmt::{Display, Formatter};

use orbit_types::tool::{McpCapability, McpTransport, ToolSessionContext};

use super::catalog::{GovernedOperation, PLUGIN_TOOL_MUTATING, PLUGIN_TOOL_READ_ONLY};
use super::env::{OPERATOR_OVERRIDE_ENV, agent_declared_in_env, env_truthy, interactive_terminal};

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
    /// An unmanaged local CLI call with no stronger identity. Only plugin tool
    /// authorization may give this caller agent access.
    LocalCli,
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
            Self::LocalCli => "local-cli",
            Self::Unknown => "unknown",
        })
    }
}

/// Which signals a surface allows to contribute to a caller's capabilities.
///
/// This is not a second authorization rule — [`super::authorize`] is unchanged by it.
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
    /// The trusted local CLI transport, used only for plugin tool authorization.
    pub local_cli: bool,
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
            local_cli: session.transport == Some(McpTransport::Local),
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
    pub(super) grants: BTreeSet<McpCapability>,
    pub(super) provenance: CallerProvenance,
    pub(super) resolution: CapabilityResolution,
    pub(super) remote_caller_machine_id: Option<String>,
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
    /// 5. **Local CLI.** An unmanaged invocation is identified but gets no
    ///    capability here. [`Self::resolve_for_operation`] grants agent only
    ///    while authorizing plugin tools.
    /// 6. **Nothing.** An unidentified caller gets an empty set, and every
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

    /// Resolve a caller for one governed operation.
    ///
    /// An unmanaged local CLI caller gains agent identity only at the plugin
    /// tool chokepoint. Both plugin rows use that identity; the mutating row
    /// still requires operator or runner and therefore denies this caller.
    pub fn resolve_for_operation(envelope: &CallerEnvelope, operation: &GovernedOperation) -> Self {
        let mut caller = Self::resolve(envelope);
        if caller.provenance == CallerProvenance::LocalCli
            && (operation.id == PLUGIN_TOOL_READ_ONLY.id || operation.id == PLUGIN_TOOL_MUTATING.id)
        {
            caller.grants.insert(McpCapability::Agent);
        }
        caller
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
        if envelope.local_cli && envelope.resolution == CapabilityResolution::ProcessEnvelope {
            return (BTreeSet::new(), CallerProvenance::LocalCli);
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
    pub(super) fn grants_label(&self) -> String {
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
