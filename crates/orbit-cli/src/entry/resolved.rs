//! What the entry resolves once from the captured `Env`, then hands to the
//! commands: the global and generation roots, the worker invocation, the
//! actor and the managed workspace selector.
//!
//! The roots are plain path arithmetic over the `Env` and `--root`, so they are
//! resolved eagerly and a failure is kept until a command asks for the root.
//! The worker invocation may wait on its protected record, so it is read on the
//! first ask and never again. Nothing here is a static: the value lives in
//! `run` and is borrowed by the invocation value (`DispatchContext`).

use std::cell::OnceCell;
use std::path::{Path, PathBuf};

use orbit_common::Env;
use orbit_core::{ActorIdentity, OrbitError, OrbitRuntime};
use orbit_types::tool::WorkerInvocation;

type WorkerCell = OnceCell<Result<Option<WorkerInvocation>, OrbitError>>;

/// The values the entry resolves from the `Env` for one invocation.
pub(crate) struct Resolved {
    global_root: Result<PathBuf, OrbitError>,
    generation_root: Result<PathBuf, OrbitError>,
    process_generation_root: Result<PathBuf, OrbitError>,
    global_orbit_dir: Result<PathBuf, OrbitError>,
    worker_invocation: WorkerCell,
    generation_worker_invocation: WorkerCell,
    actor: ActorIdentity,
    managed_workspace: Option<String>,
}

impl Resolved {
    /// Resolve the roots, the actor and the managed workspace selector. `root`
    /// is the global `--root` override.
    pub(crate) fn resolve(env: &Env, root: Option<&Path>) -> Self {
        Self {
            global_root: global_root(env),
            generation_root: generation_root(env, root),
            process_generation_root: orbit_core::runtime::resolve_process_generation_root(
                env, root,
            ),
            global_orbit_dir: orbit_common::fs::path::global_orbit_dir(env),
            worker_invocation: OnceCell::new(),
            generation_worker_invocation: OnceCell::new(),
            actor: ActorIdentity::from_env(env),
            managed_workspace: orbit_core::runtime::managed_workspace_selector_from_env(env),
        }
    }

    /// The host-global Orbit root (`~/.orbit`, or the managed run's registry root).
    pub(crate) fn global_root(&self) -> Result<&Path, OrbitError> {
        self.global_root.as_deref().map_err(replay)
    }

    /// The invoking user's `~/.orbit`, even when a managed run relocates the
    /// registry root.
    pub(crate) fn global_orbit_dir(&self) -> Result<&Path, OrbitError> {
        self.global_orbit_dir.as_deref().map_err(replay)
    }

    /// The root `--root`, `ORBIT_ROOT` or the global root selects.
    pub(crate) fn generation_root(&self) -> Result<&Path, OrbitError> {
        self.generation_root.as_deref().map_err(replay)
    }

    /// The root this process pins its executable generation to: the generation
    /// root, except for a managed macOS child, which joins its parent's registry.
    pub(crate) fn process_generation_root(&self) -> Result<&Path, OrbitError> {
        self.process_generation_root.as_deref().map_err(replay)
    }

    /// The process's worker binding, restored from the host's protected
    /// invocation record in the global root on the first ask.
    pub(crate) fn worker_binding(
        &self,
        env: &Env,
    ) -> Result<Option<&WorkerInvocation>, OrbitError> {
        restore(&self.worker_invocation, self.global_root(), env)
    }

    /// The worker binding as the MCP server reads it: from the record in the
    /// generation root, which `--root` and `ORBIT_ROOT` move.
    pub(crate) fn worker_binding_in_generation_root(
        &self,
        env: &Env,
    ) -> Result<Option<&WorkerInvocation>, OrbitError> {
        restore(
            &self.generation_worker_invocation,
            self.generation_root(),
            env,
        )
    }

    /// The actor this process acts as.
    pub(crate) fn actor(&self) -> &ActorIdentity {
        &self.actor
    }

    /// The workspace a managed run binds its commands to, when it is one.
    pub(crate) fn managed_workspace(&self) -> Option<&str> {
        self.managed_workspace.as_deref()
    }
}

/// Read the worker binding under `root` once into `cell`.
fn restore<'a>(
    cell: &'a WorkerCell,
    root: Result<&Path, OrbitError>,
    env: &Env,
) -> Result<Option<&'a WorkerInvocation>, OrbitError> {
    let restored = cell.get_or_init(|| {
        let root = root?;
        OrbitRuntime::current_worker_invocation(env, root)
    });
    match restored {
        Ok(binding) => Ok(binding.as_ref()),
        Err(error) => Err(replay(error)),
    }
}

/// The host-global Orbit root for `env`.
pub(super) fn global_root(env: &Env) -> Result<PathBuf, OrbitError> {
    orbit_core::runtime::resolve_global_root(env)
}

/// The Orbit root `root` (`--root`) or `env` selects.
pub(super) fn generation_root(env: &Env, root: Option<&Path>) -> Result<PathBuf, OrbitError> {
    orbit_core::runtime::resolve_generation_root(env, root)
}

/// A refusal as stored is not `Clone`; a command asking for a root that failed
/// to resolve gets an equal error, with the variants these resolutions raise
/// kept as they are.
fn replay(error: &OrbitError) -> OrbitError {
    match error {
        OrbitError::InvalidInput(message) => OrbitError::InvalidInput(message.clone()),
        OrbitError::WorkspaceError(message) => OrbitError::WorkspaceError(message.clone()),
        OrbitError::PolicyDenied(message) => OrbitError::PolicyDenied(message.clone()),
        OrbitError::Io(message) => OrbitError::Io(message.clone()),
        other => OrbitError::Execution(other.to_string()),
    }
}
