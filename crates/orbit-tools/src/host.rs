//! Orbit host surface a builtin tool calls instead of respawning the CLI.

use std::path::PathBuf;

use orbit_common::OrbitError;
use orbit_common::governance::auto_task::AutoTaskVerb;
use orbit_common::governance::codebase::CodebaseVerb;
use orbit_common::governance::drain::DrainVerb;
use orbit_common::governance::friction::FrictionVerb;
use orbit_common::governance::orchestrator::OrchestratorVerb;
use orbit_common::governance::search::SearchVerb;
use orbit_common::governance::task::TaskVerb;
use orbit_common::governance::workflow::WorkflowVerb;
use orbit_common::governance::workspace_claim::WorkspaceClaimVerb;
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
/// `transport` comes from the trusted caller context, never the payload:
/// MCP trims the source string; CLI and deterministic attachments (`None`)
/// select the exact filesystem name.
pub fn prepare_remote_task_artifact_put(
    input: Value,
    cwd: Option<&std::path::Path>,
    workspace_root: Option<&std::path::Path>,
    transport: Option<orbit_types::tool::McpTransport>,
) -> Result<Value, OrbitError> {
    let ctx = ToolContext {
        cwd: cwd.map(|path| path.to_string_lossy().into_owned()),
        workspace_root: workspace_root.map(PathBuf::from),
        session_context: orbit_types::tool::ToolSessionContext {
            transport,
            ..Default::default()
        },
        ..ToolContext::default()
    };
    crate::builtin::orbit::task::artifact_put::prepare_remote_payload(input, &ctx)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrbitBuiltinAction {
    /// Auto-task verbs are registry data, like friction's.
    AutoTask(AutoTaskVerb),
    AgentInvoke,
    /// Read-only codebase analytics verbs.
    Codebase(CodebaseVerb),
    CommandExec,
    DesktopRead,
    DesktopAutomation,
    DesktopTaskSnapshot,
    DesktopTaskWrite,
    /// Distributed-drain verbs, including the historical task pull tool.
    Drain(DrainVerb),
    /// ADR-0209 bearing 1 [ORB-10358]: friction verbs are registry data, so one
    /// action variant carries the verb instead of one variant per verb.
    Friction(FrictionVerb),
    /// Read-only orchestrator scorecard verbs.
    Orchestrator(OrchestratorVerb),
    PipelineInvoke,
    PipelineWait,
    /// Shared lexical search operation.
    Search(SearchVerb),
    /// Task verbs, one per task tool.
    Task(TaskVerb),
    /// Workflow-run verbs: ship, run reads and resume, and the auto-drain
    /// control.
    Workflow(WorkflowVerb),
    /// Workspace coordination-hold verbs.
    WorkspaceClaim(WorkspaceClaimVerb),
}

impl From<CodebaseVerb> for OrbitBuiltinAction {
    fn from(verb: CodebaseVerb) -> Self {
        Self::Codebase(verb)
    }
}

impl From<OrchestratorVerb> for OrbitBuiltinAction {
    fn from(verb: OrchestratorVerb) -> Self {
        Self::Orchestrator(verb)
    }
}

impl From<DrainVerb> for OrbitBuiltinAction {
    fn from(verb: DrainVerb) -> Self {
        Self::Drain(verb)
    }
}

impl From<WorkspaceClaimVerb> for OrbitBuiltinAction {
    fn from(verb: WorkspaceClaimVerb) -> Self {
        Self::WorkspaceClaim(verb)
    }
}

impl From<AutoTaskVerb> for OrbitBuiltinAction {
    fn from(verb: AutoTaskVerb) -> Self {
        Self::AutoTask(verb)
    }
}

impl From<TaskVerb> for OrbitBuiltinAction {
    fn from(verb: TaskVerb) -> Self {
        Self::Task(verb)
    }
}

impl From<FrictionVerb> for OrbitBuiltinAction {
    fn from(verb: FrictionVerb) -> Self {
        Self::Friction(verb)
    }
}

impl From<WorkflowVerb> for OrbitBuiltinAction {
    fn from(verb: WorkflowVerb) -> Self {
        Self::Workflow(verb)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OrbitTaskScope {
    pub orbit_root: Option<PathBuf>,
    pub task_id: Option<String>,
    pub run_id: Option<String>,
}

pub trait OrbitToolHost: Send + Sync {
    /// Prepare a versioned operation before shape validation: check wire
    /// compatibility and apply historical wire defaults.
    /// An incompatible protocol must be reported before a new request shape
    /// can hide it as invalid input. Hosts without such a protocol do nothing.
    fn prepare_operation_input(
        &self,
        _action: OrbitBuiltinAction,
        _input: &mut Value,
    ) -> Result<(), OrbitError> {
        Ok(())
    }

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

    /// Record that a call to `tool_name` carried input fields its operation
    /// does not declare, one audit row per field. `actor` is the caller's
    /// resolved attribution, when it has one. Hosts without an audit store
    /// record nothing.
    fn record_unknown_input_fields(
        &self,
        _tool_name: &str,
        _fields: &[String],
        _actor: Option<&str>,
    ) -> Result<(), OrbitError> {
        Ok(())
    }

    fn task_scope(&self) -> OrbitTaskScope;
}

impl From<SearchVerb> for OrbitBuiltinAction {
    fn from(verb: SearchVerb) -> Self {
        Self::Search(verb)
    }
}
