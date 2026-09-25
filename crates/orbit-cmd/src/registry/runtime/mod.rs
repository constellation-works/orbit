//! Registry-aware runtime composition and workspace selection.

#[cfg(test)]
use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use orbit_common::OrbitError;
use orbit_core::OrbitRuntime;
use orbit_core::runtime::{
    HostLifetime, OrbitRuntimeRoots, ResolvedOrbitRoots, WorkspaceRootHint,
    WorkspaceRuntimeBinding, managed_workspace_selector_from_env,
};
use orbit_store::maintenance::task_registry::{
    TaskRegistryStore, task_registry_path, workspace_config_path,
};
use orbit_types::workspace::{
    Workspace, WorkspaceCheckout, WorkspaceCheckoutRole, WorkspaceRegistry, WorkspaceStatus,
};
use serde_json::Value;

use orbit_registry::{MachineIdentityState, inspect_machine_identity, workspace_registry};

use crate::workspace_catalog::attach as attach_workspace_catalog;

mod factory;
mod selection;

pub use factory::{
    RegisteredRuntimeFactory, RegisteredRuntimeStamp, ResolvedWorkspaceBinding,
    ResolvedWorkspaceSelection, resolved_workspace_binding, workspace_runtime_binding,
};
#[cfg(test)]
pub(crate) use selection::GitProcessProbes;
#[cfg(test)]
pub(crate) use selection::retry_pipeline_worker_bootstrap;
#[cfg(test)]
pub(crate) use selection::select_workspace_for_cwd_and_roots;
#[cfg(test)]
pub(crate) use selection::sync_task_prefix;
pub use selection::{global_root_for, selector_looks_like_path, sync_runtime_task_prefix};

#[cfg(test)]
mod tests;
