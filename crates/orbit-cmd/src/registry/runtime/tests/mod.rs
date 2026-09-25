use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock, mpsc};
use std::time::{Duration, Instant};

use chrono::Utc;
use orbit_common::{NotFoundKind, OrbitError};
use orbit_core::runtime::OrbitRuntimeRoots;
use orbit_core::{
    AutoTaskAddParams, AutoTaskSchedule, AutoTaskTemplate, DedupePolicy, OrbitRuntime,
    TaskPriority, TaskStatus, TaskType,
};
use orbit_store::maintenance::task_registry::{WorkspaceConfig, write_workspace_config};
use orbit_types::workspace::{
    Workspace, WorkspaceCheckout, WorkspaceCheckoutRole, WorkspaceRegistry, WorkspaceStatus,
};
use serde_json::{Value, json};

use orbit_registry::workspace_registry::{
    self, load_registry_from, registry_path_for, save_registry_to,
};

use crate::registry_runtime::{
    GitProcessProbes, RegisteredRuntimeFactory, ResolvedWorkspaceSelection,
    resolved_workspace_binding, retry_pipeline_worker_bootstrap,
    select_workspace_for_cwd_and_roots, sync_task_prefix, workspace_runtime_binding,
};

mod bootstrap;
mod paths;
mod selection;

use bootstrap::*;
use selection::*;
