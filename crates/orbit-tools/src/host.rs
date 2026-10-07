//! Orbit host surface a builtin tool calls instead of respawning the CLI.

use std::path::PathBuf;

use orbit_common::OrbitError;
use orbit_common::governance::friction::FrictionVerb;
use serde_json::Value;

use super::context::{ReservationOwnerContext, ToolContext};

/// Owner transport injected by composition. Implementations verify the
/// destination identity and never substitute an execution-host store.
pub trait OwnerCoordinator: Send + Sync {
    fn call(
        &self,
        name: &str,
        input: Value,
        session: orbit_types::tool::ToolSessionContext,
    ) -> Result<Value, OrbitError>;
}

/// A follower drain's transport to its owner, injected by composition
/// [ORB-13625].
///
/// It carries the distributed drain's own protocol — probe, pull, bind,
/// settle and receipt lookup — which a drain coordinator speaks before any
/// worker binding exists, so it cannot ride [`OwnerCoordinator`], whose every
/// call is a bound worker's. Implementations deliver to the destination the
/// host-qualified selector names and never answer from a local store.
pub trait DrainOwnerTransport: Send + Sync {
    /// Deliver `name` to the owner workspace `selector` names.
    fn call(&self, selector: &str, name: &str, input: Value) -> Result<Value, OrbitError>;

    /// Read `orbit.task.show` from the owner workspace `selector` names.
    ///
    /// The drain protocol route carries only the drain's own rows, so a
    /// follower that must read a task — worktree GC deciding whether a
    /// claimed leaf's task has settled — asks over the owner's ordinary tool
    /// surface instead [ORB-13920].
    fn show_task(&self, selector: &str, input: Value) -> Result<Value, OrbitError>;

    /// The `machine_id` of the registered host whose task prefix is
    /// `prefix`, read from the host file; `None` when no host claims it
    /// [ORB-14449]. A transport without a host file knows no prefixes.
    fn task_prefix_host(&self, _prefix: &str) -> Result<Option<String>, OrbitError> {
        Ok(None)
    }

    /// Read `orbit.task.show` id-only from the host the id's prefix names.
    ///
    /// The call carries no selector; the destination resolves the id through
    /// its own task registry.
    fn show_task_by_id(&self, _input: Value) -> Result<Value, OrbitError> {
        Err(OrbitError::host_registry(
            orbit_common::HostRegistryCode::UnknownTaskPrefix,
            "this transport does not route task ids by prefix",
        ))
    }

    /// The coordinator a claimed leaf bound to this transport's owner routes
    /// its worker reads and writes through.
    fn worker_coordinator(&self) -> std::sync::Arc<dyn OwnerCoordinator>;
}

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
    crate::builtin::orbit::task::artifact_put::prepare_remote_payload(input, &ctx)
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
    AgentInvoke,
    CommandExec,
    DesktopRead,
    DesktopDrain,
    DesktopAutomation,
    DesktopTaskSnapshot,
    DesktopTaskWrite,
    DrainClaimBind,
    DrainClaimSettle,
    DrainClaims,
    DrainProbe,
    DrainReceiptLookup,
    /// ADR-0209 bearing 1 [ORB-10358]: friction verbs are registry data, so one
    /// action variant carries the verb instead of one variant per verb.
    Friction(FrictionVerb),
    PipelineInvoke,
    PipelineWait,
    Search,
    StateGet,
    StateSet,
    TaskAdd,
    TaskArtifactGet,
    TaskDelete,
    TaskEligible,
    TaskLint,
    TaskList,
    TaskLocks,
    TaskLocksRelease,
    TaskLocksReserve,
    TaskPull,
    TaskReject,
    TaskReconcileReview,
    TaskReviewReset,
    TaskShow,
    TaskUpdate,
    WorkflowRunList,
    WorkflowRunResume,
    WorkflowRunShow,
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
