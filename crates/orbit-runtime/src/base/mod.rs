//! [`Base`]: what every operation has — the resolved configuration, the
//! paths, the actor, the clock, the event log, the host OS and the workspace
//! this process was opened for.
//!
//! The lowest of the four runtime layers. It holds no store and nothing that
//! belongs to one machine's work or one run.

use std::any::Any;
use std::path::Path;
use std::sync::Arc;

use orbit_config::{CodexExecutionPolicy, ExecutionEnvPolicy, PersistenceConfig};
use orbit_policy::PolicyEngine;
use orbit_types::workspace::WorkspacePaths;

use crate::builder;
use crate::context::{ActorIdentity, OrbitPolicyContext, OrbitRuntimeSettings};
use crate::event_bus;
use crate::workspace::binding::WorkspaceRuntimeBinding;

/// Configuration reload supplied by the composition root. Runtime policy
/// refreshes may call it after config files change, but runtime never opens
/// config layers itself.
pub type RuntimeConfigLoader =
    Arc<dyn Fn() -> Result<orbit_config::ResolvedConfig, orbit_common::OrbitError> + Send + Sync>;

/// The layer every operation runs against.
#[derive(Clone)]
pub struct Base {
    pub(crate) paths: WorkspacePaths,
    pub(crate) policy: OrbitPolicyContext,
    /// The admitted settings, which carry the actor.
    pub(crate) settings: OrbitRuntimeSettings,
    pub(crate) config_loader: RuntimeConfigLoader,
    pub(crate) event_log: event_bus::EventLog,
    /// The operating system admission matches a task's `os:` tags against.
    /// This binary's own by default; a fixture composes another so one process
    /// can stand in for a host of each OS.
    pub(crate) host_os: Option<orbit_types::task::HostOs>,
    /// Registered remote hosts by machine id with their recorded operating
    /// system, supplied by the registry-owning composition layer.
    /// Registration alone does not make a host serve this workspace: a
    /// registered host counts for creation-time complexity draws only when
    /// this workspace's own claim records show it pulled work recently (see
    /// `serving_host_oses`). Admission and activity draws keep checking
    /// only `host_os`. Empty on a standalone runtime, which then
    /// draws exactly as a single-host owner always has.
    pub(crate) registered_host_oses: Vec<(String, orbit_types::task::HostOs)>,
    pub(crate) workspace_binding: Option<Arc<WorkspaceRuntimeBinding>>,
    /// The workspace was named by an explicit selector rather than discovered
    /// from the caller's working directory. Supplied by the composition layer
    /// that resolved the selector; never inferred by the runtime.
    pub(crate) workspace_explicitly_selected: bool,
    /// Registry identity `(id, name)` of this runtime's checkout, resolved
    /// from the `workspaces.json` the registry-owning layer loaded for this
    /// open. The outer `None` means the open carried no registry; the inner
    /// `None` is a recorded miss (an unregistered checkout).
    pub(crate) opened_registered_workspace: Option<Option<(String, String)>>,
    /// Data the registry-owning composition layer already loaded while opening
    /// this runtime. Opaque here: the runtime does not inspect it and does not
    /// load config to refill it. Composition downcasts its own snapshot.
    pub(crate) composition_snapshot: Option<Arc<dyn Any + Send + Sync>>,
    /// The observation clock for deterministic CI-failure filing. `None` is
    /// the current UTC time.
    pub(crate) ci_failure_time: Option<chrono::DateTime<chrono::Utc>>,
    /// Keeps an in-memory runtime's roots alive. Declared last so every path
    /// above is dropped before the directory is removed.
    _temp_dir: Option<Arc<builder::TempDir>>,
}

impl Base {
    /// A base as a runtime opens it: this binary's host OS, an empty event
    /// log and nothing yet supplied by composition.
    pub(crate) fn new(
        paths: WorkspacePaths,
        policy: OrbitPolicyContext,
        settings: OrbitRuntimeSettings,
        config_loader: RuntimeConfigLoader,
        workspace_binding: Option<Arc<WorkspaceRuntimeBinding>>,
        temp_dir: Option<Arc<builder::TempDir>>,
    ) -> Self {
        Self {
            paths,
            policy,
            settings,
            config_loader,
            event_log: event_bus::EventLog::default(),
            host_os: orbit_types::task::HostOs::current(),
            registered_host_oses: Vec::new(),
            workspace_binding,
            workspace_explicitly_selected: false,
            opened_registered_workspace: None,
            composition_snapshot: None,
            ci_failure_time: None,
            _temp_dir: temp_dir,
        }
    }

    /// Returns the shared .orbit/ data directory.
    pub(crate) fn shared_root(&self) -> &Path {
        &self.paths.orbit_dir
    }

    /// Returns the worktree-local .orbit/ data directory.
    pub(crate) fn local_root(&self) -> &Path {
        &self.paths.local_dir
    }

    pub(crate) fn global_root(&self) -> &Path {
        &self.paths.global_dir
    }

    /// The resolved workspace paths.
    pub fn paths(&self) -> &WorkspacePaths {
        &self.paths
    }

    pub(crate) fn policy(&self) -> &PolicyEngine {
        &self.policy.policy
    }

    pub(crate) fn execution_env_policy(&self) -> &ExecutionEnvPolicy {
        &self.policy.execution_env_policy
    }

    pub(crate) fn apply_child_env_defaults(&mut self, defaults: &[(String, String)]) {
        self.policy.execution_env_policy = self
            .policy
            .execution_env_policy
            .clone()
            .with_defaults(defaults);
    }

    pub(crate) fn codex_execution_policy(&self) -> &CodexExecutionPolicy {
        &self.policy.codex_execution_policy
    }

    /// Where this workspace's state is persisted.
    pub fn persistence(&self) -> &PersistenceConfig {
        &self.settings.persistence
    }

    pub(crate) fn actor(&self) -> &ActorIdentity {
        &self.settings.actor
    }

    pub(crate) fn set_actor(&mut self, actor: ActorIdentity) {
        self.settings.actor = actor;
    }

    /// The admitted settings.
    pub fn settings(&self) -> &OrbitRuntimeSettings {
        &self.settings
    }
}
