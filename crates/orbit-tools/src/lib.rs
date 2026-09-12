#![deny(clippy::print_stderr, clippy::print_stdout)]
// Legacy tool-registry surfaces still need a focused documentation pass.
#![allow(missing_docs)]
// Unit tests use unwrap/expect for fixture setup; production call sites remain linted.
#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used))]
#![allow(
    rustdoc::broken_intra_doc_links,
    rustdoc::invalid_html_tags,
    rustdoc::private_intra_doc_links
)]
//! Builtin tool registry providing the standard Orbit toolset for agents and jobs.
//!
//! Implements and registers all built-in tools that agents can invoke during
//! activity execution: git, GitHub, Orbit CLI, process, time, and
//! network tools. External (user-defined) tools are also supported via the registry.
//!
//! # Role
//! Depends on `orbit-exec` for process spawning and `orbit-common` for shared
//! types. Consumed by `orbit-engine`, `orbit-core`, and `orbit-mcp`, which
//! composes its workspace-scoped definitions with MCP discovery tools.
//!
//! # Key exports
//! - [`ToolRegistry`] — central registry; call `register_builtins()` to load all standard tools
//! - [`Tool`] trait — implement this to add a custom tool
//! - [`ToolContext`] — per-call context: cwd, allowed-tool allowlist, workspace root boundary
//! - [`require_str`] — helper to extract and validate string fields from tool input JSON
//! - [`check_exec_result`] — helper to turn a failed [`ExecutionResult`] into an `OrbitError`
//! - Timeout constants: [`TIMEOUT_FAST_MS`], [`TIMEOUT_DEFAULT_MS`], [`TIMEOUT_SLOW_MS`], [`TIMEOUT_LONG_MS`]
//!
//! # Registry contents
//! The builtin registry wires together the standard Orbit tool families:
//! git and GitHub helpers, Orbit task/job commands, process spawning,
//! network fetches, and time utilities. Each tool executes inside a
//! [`ToolContext`] that carries workspace boundaries, agent metadata,
//! process allowlists, and the narrow Orbit host surface used by Orbit builtins.
//!
//! # Dependency direction
//! orbit-common / orbit-exec / orbit-policy → `orbit-tools` → orbit-engine, orbit-core, orbit-mcp

pub(crate) mod builtin;
pub mod external;
pub mod github_cli;
mod registry;

use std::path::PathBuf;
use std::sync::Arc;

use orbit_policy::PolicyEngine;
use serde_json::{Map, Value};

use orbit_common::OrbitError;
use orbit_common::governance::friction::FrictionVerb;
use orbit_common::governance::operation_mode::OperationModeVerb;
use orbit_types::tool::ToolSchema;

/// Fast operation timeout (1 s). Used for local command resolution (e.g. `which`).
pub const TIMEOUT_FAST_MS: u64 = 1_000;

/// Default network operation timeout (15 s). Used for most GitHub API calls
/// and Orbit CLI commands where a quick response is expected.
pub const TIMEOUT_DEFAULT_MS: u64 = 15_000;

/// Slow operation timeout (30 s). Used for git network operations and PR creation,
/// which may involve larger payloads or slower remotes.
pub const TIMEOUT_SLOW_MS: u64 = 30_000;

/// Long operation timeout (60 s). Used for `gh pr checkout`, which clones or
/// fetches a branch and may transfer significant data over the network.
pub const TIMEOUT_LONG_MS: u64 = 60_000;

pub use registry::{ToolRegistry, canonical_builtin_mcp_tool_definitions};

/// Materialize a task artifact before a spoke sends the coordination request
/// to its hub. The returned value contains artifact bytes but no source path.
///
/// `source_path` is resolved against `cwd` and must land inside
/// `workspace_root` after symlink resolution. The hub never sees the path.
pub fn prepare_remote_task_artifact_put(
    input: Value,
    cwd: Option<&std::path::Path>,
    workspace_root: Option<&std::path::Path>,
) -> Result<Value, OrbitError> {
    let ctx = ToolContext {
        cwd: cwd.map(|path| path.to_string_lossy().into_owned()),
        workspace_root: workspace_root.map(PathBuf::from),
        ..ToolContext::default()
    };
    builtin::orbit::task::artifact_put::prepare_remote_payload(input, &ctx)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrbitBuiltinAction {
    AdrAdd,
    AdrShow,
    AdrList,
    AdrRestore,
    AdrUpdate,
    AdrSupersede,
    AutoTaskAdd,
    AutoTaskList,
    AutoTaskMint,
    AutoTaskShow,
    AutoTaskUpdate,
    AutoTaskToggle,
    AgentInvoke,
    CommandExec,
    DocsList,
    DocsShow,
    DocsAdd,
    DocsIndex,
    DocsMigrate,
    /// ADR-0209 bearing 1 [ORB-10358]: friction verbs are registry data, so one
    /// action variant carries the verb instead of one variant per verb.
    Friction(FrictionVerb),
    /// Operation-mode verbs are registry data too [ORB-11332].
    OperationMode(OperationModeVerb),
    PipelineInvoke,
    PipelineWait,
    Search,
    SemanticIndex,
    SemanticInstall,
    SemanticStats,
    SemanticUninstall,
    StateGet,
    StateSet,
    TaskAdd,
    TaskArtifactGet,
    TaskDelete,
    TaskLint,
    TaskList,
    TaskLocks,
    TaskLocksRelease,
    TaskLocksReserve,
    TaskReject,
    TaskShow,
    TaskUpdate,
    WorkflowRunList,
    WorkflowRunResume,
    WorkflowRunShow,
    WorkflowRunWorkers,
    WorkflowShip,
    WorkspaceClaimAcquire,
    WorkspaceClaimRelease,
    WorkspaceClaimShow,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OrbitTaskScope {
    pub orbit_root: Option<PathBuf>,
    pub task_id: Option<String>,
    pub run_id: Option<String>,
}

pub trait OrbitToolHost: Send + Sync {
    fn execute(
        &self,
        action: OrbitBuiltinAction,
        input: Value,
        agent: Option<String>,
        model: Option<String>,
        reservation_owner: Option<ReservationOwnerContext>,
    ) -> Result<Value, OrbitError>;

    /// Execute with an actor asserted by the in-process host rather than tool
    /// input. Implementations that do not distinguish trust reuse `execute`.
    fn execute_with_trusted_actor(
        &self,
        action: OrbitBuiltinAction,
        input: Value,
        actor_label: String,
        reservation_owner: Option<ReservationOwnerContext>,
    ) -> Result<Value, OrbitError> {
        self.execute(action, input, None, Some(actor_label), reservation_owner)
    }

    fn task_scope(&self) -> OrbitTaskScope;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsCallEventKind {
    Request,
    Result,
    Denied,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsCallEvent {
    pub kind: FsCallEventKind,
    pub profile: String,
    pub op: String,
    pub path: String,
    pub allowed: bool,
    pub matched_rule: String,
}

pub trait FsAuditLogger: Send + Sync {
    fn emit(&self, event: FsCallEvent) -> Result<(), OrbitError>;
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReservationOwnerContext {
    pub owner_run_id: String,
    pub owner_metadata_json: Option<String>,
}

#[derive(Clone, Default)]
pub struct ToolContext {
    pub cwd: Option<String>,
    /// Ambient metadata asserted by the transport/session, not by tool input.
    pub session_context: orbit_types::tool::ToolSessionContext,
    /// If non-empty, only tools in this list may be called. Empty means unrestricted.
    pub allowed_tools: Vec<String>,
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
    /// Program allowlist for `proc.spawn`. When `proc_spawn_activity_scoped`
    /// is `true`, an empty list denies every program (fail-closed). When
    /// `proc_spawn_activity_scoped` is `false`, an empty list preserves the
    /// legacy unrestricted behaviour for direct CLI / v1 callers.
    pub proc_allowed_programs: Vec<String>,
    /// Complete environment for `proc.spawn`, resolved from the operator's
    /// execution environment policy by the runtime. `None` uses Orbit's
    /// credential-free child baseline.
    pub proc_spawn_environment: Option<Vec<(String, String)>>,
    /// True when `proc.spawn` runs inside an activity-scoped context. Every v2
    /// activity context sets it, so an empty `proc_allowed_programs` denies
    /// every program (fail-closed) rather than degrading to allow-all when an
    /// asset omits the key ([ORB-10959]). Only direct CLI / v1 callers leave it
    /// `false`.
    pub proc_spawn_activity_scoped: bool,
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
            .field("has_policy_engine", &self.policy_engine.is_some())
            .field("fs_profile", &self.fs_profile)
            .field("reservation_owner", &self.reservation_owner)
            .field("has_orbit_host", &self.orbit_host.is_some())
            .finish()
    }
}

pub trait Tool: Send + Sync {
    fn schema(&self) -> ToolSchema;
    /// Whether a successful invocation can have externally visible side effects.
    ///
    /// The conservative default keeps audit persistence fail-closed for every
    /// tool that has not explicitly proved it is observational only.
    fn execution_kind(&self) -> ToolExecutionKind {
        ToolExecutionKind::Mutating
    }
    fn execute(&self, ctx: &ToolContext, input: Value) -> Result<Value, OrbitError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolExecutionKind {
    ReadOnly,
    Mutating,
}

/// Extract a non-empty string field from a tool input value.
///
/// Returns `Err(OrbitError::InvalidInput)` if the key is absent, not a string,
/// or contains only whitespace. The returned string is trimmed.
pub fn require_str(input: &Value, key: &str) -> Result<String, OrbitError> {
    let value = input
        .get(key)
        .ok_or_else(|| OrbitError::InvalidInput(format!("missing `{key}`")))?;
    // Accept both strings and numbers (agents often pass numeric IDs without quotes).
    let raw = match value {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => return Err(OrbitError::InvalidInput(format!("missing `{key}`"))),
    };
    let trimmed = raw.trim().to_string();
    if trimmed.is_empty() {
        return Err(OrbitError::InvalidInput(format!("missing `{key}`")));
    }
    Ok(trimmed)
}

/// Assert that a process result succeeded, returning a descriptive error if not.
///
/// Use this instead of the repeated `if !result.success { return Err(...) }` pattern.
/// The `label` should be the command name (e.g. `"gh pr comment"`) and is included
/// in the error message for diagnostics.
pub fn check_exec_result(
    result: &orbit_types::tool::ExecutionResult,
    label: &str,
) -> Result<(), OrbitError> {
    if result.success {
        Ok(())
    } else {
        Err(OrbitError::Execution(format!(
            "{label} failed: {}",
            result.stderr.trim()
        )))
    }
}

pub fn map_input_from_pairs(pairs: impl IntoIterator<Item = (String, String)>) -> Value {
    let mut map = Map::new();
    for (key, value) in pairs {
        map.insert(key, Value::String(value));
    }
    Value::Object(map)
}
