//! Per-call tool context and the caller, binding, and grant types it carries.

use std::path::PathBuf;
use std::sync::Arc;

use orbit_policy::PolicyEngine;

use super::fs_audit::FsAuditLogger;
use super::host::OrbitToolHost;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReservationOwnerContext {
    pub owner_run_id: String,
    pub owner_metadata_json: Option<String>,
}

/// The managed activity a tool call serves, attested by the host that
/// dispatched it.
///
/// Built only from the dispatcher's own run binding: the in-process activity
/// context, or the managed-run environment Orbit stamps into a CLI agent it
/// spawns (`ORBIT_MANAGED_RUN_CONTEXT` with `ORBIT_RUN_ID` and
/// `ORBIT_TASK_ID`). Tool input never contributes to it, and an interactive
/// call has none. Plugin backends receive it as `task_id` / `job_run_id` in
/// their call context (`plugin::envelope`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ActivityBinding {
    /// The job run the activity belongs to.
    pub job_run_id: String,
    /// The task the activity is bound to, when it serves one.
    pub task_id: Option<String>,
}

/// Who chose a tool call, as the host that built the context attests it.
///
/// It decides one thing today: what bounds the programs a plugin backend
/// declares it spawns (`requires.programs`, design
/// `docs/design/plugins/1_scope.md` §4.3). Tool input never sets it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ToolCaller {
    /// An agent, an interactive client, or any context no host attested
    /// otherwise. A plugin's declared programs are held to
    /// the activity's program policy exactly as `proc.spawn` is, so a
    /// legacy activity-scoped context with an empty allowlist denies every one.
    #[default]
    Agent,
    /// A deterministic job step: the activity asset fixed the call, and no
    /// agent chooses it or its input. A plugin's declared programs are bounded
    /// by what the operator granted at `orbit plugin enable`, re-read from the
    /// grants witness for this call, instead of by `proc_allowed_programs`,
    /// which stays the fail-closed `proc.spawn` list.
    DeterministicStep(DeterministicStepPrograms),
}

/// The program bound of a [`ToolCaller::DeterministicStep`] call.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeterministicStepPrograms {
    /// The step activity's own program allowlist, when it declares one; a
    /// plugin's granted programs are intersected with it. `None` adds no
    /// bound beyond the grant.
    pub activity_allowed_programs: Option<Vec<String>>,
    /// The called plugin's program grant as the dispatching host re-read it.
    /// `None` until a dispatcher that knows the plugin verified it, which
    /// refuses every declared program.
    pub witnessed: Option<WitnessedProgramGrant>,
}

/// One plugin's program grant, read back from its grants witness at call
/// time rather than taken from the registry built at load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WitnessedProgramGrant {
    /// The plugin namespace the witness was read for.
    pub plugin: String,
    /// Declared program name → the canonical path recorded at consent, or why
    /// the host could not verify the grant for this call.
    pub programs: Result<std::collections::BTreeMap<String, PathBuf>, String>,
}

/// The wall-clock budget a managed activity grants `proc.spawn`.
///
/// Built only by the host that dispatched the activity: from the deadline the
/// CLI runner stamped into the provider's environment, and the operator's
/// `execution.proc_spawn_max_timeout_minutes`. Tool input never contributes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcSpawnBudget {
    /// When the activity's provider subprocess is stopped.
    pub deadline: std::time::SystemTime,
    /// Operator ceiling for one `proc.spawn` call, however long the activity
    /// has left.
    pub max_timeout_ms: u64,
}

#[derive(Clone, Default)]
pub struct ToolContext {
    pub cwd: Option<String>,
    /// Ambient metadata asserted by the transport/session, not by tool input.
    pub session_context: orbit_types::tool::ToolSessionContext,
    /// If non-empty, only tools in this list may be called. Empty means unrestricted.
    pub allowed_tools: Vec<String>,
    /// A deny-mode activity's disallow list: every tool it covers is refused,
    /// whatever `allowed_tools` says. `None` outside a deny-mode managed run.
    pub tool_deny_policy: Option<orbit_types::workflow::ActivityToolDenyPolicy>,
    /// Workspace root used by tools that enforce path containment.
    /// The runtime pipeline auto-populates this from the data root's parent directory.
    pub workspace_root: Option<PathBuf>,
    /// Normalized agent name (e.g. `"claude"`). When set, GitHub tools auto-append
    /// an attribution footer to PR bodies and review comments.
    pub agent_name: Option<String>,
    /// Resolved model identifier (e.g. `"opus-4.6"`). Used alongside `agent_name`
    /// for the attribution footer.
    pub model_name: Option<String>,
    /// Trusted actor supplied by an in-process host adapter. This is separate
    /// from agent-controlled tool input so human and system writes do not
    /// weaken canonical agent-family validation at the public tool boundary.
    pub trusted_actor_label: Option<String>,
    /// Legacy program allowlist for `proc.spawn`. In the absence of
    /// `proc_disallowed_programs`, an empty scoped list denies every program.
    /// An empty unscoped list preserves direct CLI / v1 behavior.
    pub proc_allowed_programs: Vec<String>,
    /// Present only for an activity that selects program deny mode. An empty
    /// list admits every program; absence preserves the legacy allowlist.
    pub proc_disallowed_programs: Option<Vec<String>>,
    /// Complete environment for `proc.spawn`, resolved from the operator's
    /// execution environment policy by the runtime. `None` uses Orbit's
    /// credential-free child baseline.
    pub proc_spawn_environment: Option<Vec<(String, String)>>,
    /// True when `proc.spawn` runs inside an activity-scoped context. Every v2
    /// activity context sets it, so a missing program policy denies every
    /// program (fail-closed) rather than degrading to allow-all when an asset
    /// omits the key ([ORB-10959]). Only direct CLI / v1 callers leave it
    /// `false`.
    pub proc_spawn_activity_scoped: bool,
    /// The managed activity's budget for one `proc.spawn` call. `None` keeps
    /// the 60 s ceiling: an interactive call, or an activity whose host
    /// attests no deadline.
    pub proc_spawn_budget: Option<ProcSpawnBudget>,
    /// Who chose this call. Only a host that dispatches a deterministic step
    /// sets anything but [`ToolCaller::Agent`].
    pub caller: ToolCaller,
    /// Filesystem policy engine used by Orbit-managed agent runtimes.
    pub policy_engine: Option<Arc<PolicyEngine>>,
    /// Active activity fsProfile name. Used by the CLI sandbox compiler and
    /// retained for historical `FsCallEvent` plumbing. `None` bypasses profile checks.
    pub fs_profile: Option<String>,
    /// Optional audit hook for historical / future-harness `FsCallEvent` emission.
    /// No shipped builtin currently emits these events.
    pub fs_audit: Option<Arc<dyn FsAuditLogger>>,
    /// Trusted runtime-owned reservation metadata. Tool inputs cannot set this;
    /// only Orbit dispatch context or Orbit-managed CLI environments can.
    pub reservation_owner: Option<ReservationOwnerContext>,
    /// Host-attested managed activity this call serves. `None` for an
    /// interactive call. Tool input cannot set it.
    pub activity_binding: Option<ActivityBinding>,
    /// Set only by a run's plugin broker, for a call it executes on a
    /// sandboxed agent's behalf: a plugin backend is then confined by the
    /// plugin profile narrowed to this caller
    /// ([`crate::plugin::PluginBackendSpec::brokered_sandbox_profile`]). Tool input
    /// cannot set it.
    pub brokered_caller: Option<crate::plugin::BrokeredCaller>,
    /// Session ownership and cancellation for a host-brokered call.
    pub broker_call: Option<crate::plugin::BrokerCall>,
    /// Narrow Orbit application host used by Orbit builtins instead of respawning
    /// the Orbit CLI or carrying task-specific state in the generic tool context.
    pub orbit_host: Option<Arc<dyn OrbitToolHost>>,
}

impl std::fmt::Debug for ToolContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolContext")
            .field("cwd", &self.cwd)
            .field("session_context", &self.session_context)
            .field("allowed_tools", &self.allowed_tools)
            .field("tool_deny_policy", &self.tool_deny_policy)
            .field("workspace_root", &self.workspace_root)
            .field("agent_name", &self.agent_name)
            .field("model_name", &self.model_name)
            .field("trusted_actor_label", &self.trusted_actor_label)
            .field("proc_allowed_programs", &self.proc_allowed_programs)
            .field(
                "has_proc_spawn_environment",
                &self.proc_spawn_environment.is_some(),
            )
            .field(
                "proc_spawn_activity_scoped",
                &self.proc_spawn_activity_scoped,
            )
            .field("proc_spawn_budget", &self.proc_spawn_budget)
            .field("caller", &self.caller)
            .field("has_policy_engine", &self.policy_engine.is_some())
            .field("fs_profile", &self.fs_profile)
            .field("reservation_owner", &self.reservation_owner)
            .field("activity_binding", &self.activity_binding)
            .field("brokered_caller", &self.brokered_caller)
            .field("has_orbit_host", &self.orbit_host.is_some())
            .finish()
    }
}
