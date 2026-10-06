//! Per-run plugin broker authorization and lifetime boundary.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_tools::plugin::BrokeredCaller;
use orbit_types::workflow::ActivityToolDenyPolicy;

/// Environment name that tells a sandboxed agent where its run's plugin broker
/// listens. The value locates the socket and proves nothing: the broker
/// authenticates each connection by the kernel's peer identity.
pub use orbit_common::security::child_env::PLUGIN_BROKER_ENV;

/// The dispatching run as its plugin broker authorizes and executes brokered
/// calls (`docs/design/plugins/2_agent_call_broker.md` §4.3). Every field
/// comes from the run the host dispatched; none is read from a broker request
/// or from the agent's environment.
#[derive(Debug, Clone)]
pub struct PluginBrokerRun {
    /// The run the broker serves, for its logs.
    pub run_id: String,
    /// The job run a backend is told it serves (`context.job_run_id`). `None`
    /// for an invocation without job-run authority, such as a source
    /// inspection.
    pub job_run_id: Option<String>,
    /// The task a backend is told it serves (`context.task_id`).
    pub task_id: Option<String>,
    pub activity_name: String,
    /// The provider the run dispatched, and its model.
    pub agent_name: Option<String>,
    pub model_name: Option<String>,
    /// The run's logical workspace. A request may name only this one.
    pub workspace: Option<String>,
    /// Allowlist mode: the activity's effective tools. The broker treats an
    /// empty list as "no tool", never as unrestricted.
    pub allowed_tools: Vec<String>,
    /// Deny mode: the activity's disallow list, which decides in place of
    /// `allowed_tools`.
    pub tool_deny_policy: Option<ActivityToolDenyPolicy>,
    /// The worktree, sandbox profile and program policy the agent runs
    /// under; a brokered backend is confined to them as well as to its own
    /// profile (design §5).
    pub caller: BrokeredCaller,
}

/// A per-run plugin broker a host started for one sandboxed provider launch
/// (`docs/design/plugins/2_agent_call_broker.md`).
///
/// The listener runs until the handle is dropped. Dropping it stops the
/// listener and removes the socket and the directory holding it.
pub trait PluginBrokerHandle: Send {
    /// Absolute socket path exported to the agent as [`PLUGIN_BROKER_ENV`].
    fn socket_path(&self) -> &Path;

    /// Anchor peer authentication to the sandbox process just spawned for
    /// this run: the `bwrap` child on Linux, the `sandbox-exec` child on
    /// macOS. Until this succeeds, the broker refuses every connection.
    fn bind_sandbox(&self, sandbox_pid: u32) -> Result<(), OrbitError>;
}
