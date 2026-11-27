//! Registry-aware runtime composition and workspace selection.

mod callback_workspace;
mod factory;
mod selection;

use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use orbit_common::OrbitError;
use orbit_core::app::workspace::Opening;
use orbit_runtime::{
    HostLifetime, OrbitRuntimeRoots, ResolvedOrbitRoots, WorkspaceRootHint,
    WorkspaceRuntimeBinding, managed_workspace_selector_from_env,
};
use orbit_store::maintenance::task_registry::workspace_config_path;
use orbit_types::workspace::{
    Workspace, WorkspaceCheckout, WorkspaceCheckoutRole, WorkspaceRegistry, WorkspaceStatus,
};
use serde_json::Value;

use orbit_registry::{MachineIdentityState, inspect_machine_identity, workspace_registry};

use crate::workspace_catalog::attach as attach_workspace_catalog;

pub use callback_workspace::refuse_cli_callback_workspace;
pub use factory::{
    ConfigLayerStamp, RegisteredRuntimeFactory, RegisteredRuntimeStamp, ResolvedWorkspaceBinding,
    ResolvedWorkspaceSelection, workspace_runtime_binding,
};
pub use selection::{
    PreOpenRoute, PreOpenWorkerBinding, global_root_for, replay_refusal, selector_looks_like_path,
};
