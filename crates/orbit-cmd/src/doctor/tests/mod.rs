//! Sibling tests for workspace doctor checks.

use std::fs;
use std::path::Path;

use chrono::Utc;
use fs2::FileExt;
use orbit_registry::workspace_registry;
use orbit_store::maintenance::task_registry::{
    BindWorkspaceParams, RegisterWorkspaceParams, TaskRegistryStore, task_registry_path,
    task_workspaces_dir,
};
use orbit_types::workflow::{JobRun, JobRunState};
use orbit_types::workspace::{Workspace, WorkspaceCheckout, WorkspaceStatus};
use sha2::{Digest, Sha256};

use orbit_core::OrbitRuntime;
use orbit_core::runtime::OrbitRuntimeRoots;
use orbit_store::TaskReservationReserveParams;

use crate::doctor::{
    DoctorCommands, OrphanTaskStoreRemoval, WorkspaceDoctorResult, WorkspaceDoctorStatus,
    collect_lock_files, disk_space_check, process_is_alive,
};
use crate::task_store::{partition_is_bound, retain_task_store_on_catalog_remove};

mod automation;
mod task;
mod workspace;

use workspace::*;
