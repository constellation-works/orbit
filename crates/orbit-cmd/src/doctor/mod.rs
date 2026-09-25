//! Workspace-level self-diagnostics behind `orbit doctor` [ORB-10005].
//!
//! Complements the narrower `orbit skill doctor` / `orbit tool doctor`
//! surfaces with whole-workspace checks: config validity, store database
//! integrity and schema-ledger version, free disk space on the volume
//! holding `.orbit`, search-index coverage, leftover lock
//! files from crashed holders, orphaned `running`/`pending` job runs, task
//! reservations whose owner or terminal task association is conclusively
//! inactive, task
//! relation/dependency targets that no longer resolve in the registry
//! (grandfathered relations that block index rebuilds — ORB-10305), and
//! unpublished `ORB-*` stub directories that never received `task.yaml`
//! and hold no bundle content (aborted creates that used to fail
//! `orbit task reindex` closed), plus data-bearing `ORB-*` directories
//! missing `task.yaml` (retained unresolved task data, not stubs).
//!
//! Every check degrades rather than errors: subsystems that are absent in a
//! fresh workspace report [`WorkspaceDoctorStatus::Skipped`], and probe
//! failures become `Warning`/`Error` rows instead of aborting the whole
//! diagnosis. The cheap probes shared with the dashboard's
//! `/healthz?detailed=true` ([`DoctorCommands::health_check_store_writable`])
//! also live here.

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};

use fs2::FileExt;
use orbit_common::OrbitError;
use orbit_core::OrbitRuntime;
use orbit_core::application::artifact_health::{ArtifactFinding, RetiredActivityBackendRepair};
use orbit_store::maintenance::migration::SUPPORTED_SCHEMA_VERSION;
use orbit_types::task::{TASK_ENVELOPE_FILE_NAME, is_valid_orb_task_id};
use orbit_types::workspace::WorkspacePaths;
use serde::Serialize;

use crate::task_store;

mod automation;
mod commands;
mod system;
mod task;
mod workspace;

use automation::*;
use commands::*;
pub use commands::{
    DoctorCommands, OrphanTaskStoreRemoval, WorkspaceDoctorResult, WorkspaceDoctorStatus,
};
pub(crate) use system::{collect_lock_files, disk_space_check, process_is_alive};
use task::*;
use workspace::*;

#[cfg(test)]
mod tests;
