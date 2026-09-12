use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

#[cfg(not(unix))]
use std::fs::OpenOptions;
#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd};
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

use chrono::Utc;
use orbit_common::fs::io::atomic_write_text;
use orbit_common::observability::audit_id::audit_execution_id;
use orbit_common::{NotFoundKind, OrbitError};
use orbit_store::contracts::{
    AuditEventInsertParams, ChildJobRunAdmissionOutcome, ChildJobRunAdmissionParams,
    JobRunStepParams, TaskReservationReleaseReason,
};
use orbit_types::record::OrbitEvent;
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::workflow::{
    JobRun, JobRunStartOutcome, JobRunState, JobRunTrigger, JobScheduleState, JobTargetType,
};
use orbit_types::workspace::WorkspacePaths;
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use orbit_engine::activity_job::load_job_asset;
use orbit_types::workflow::JobV2;
use orbit_types::workflow::activity_job::{
    TRUSTED_HOST_ADMISSION_KEY, run_input_declares_trusted_host, validate_job_retired_sessions,
};

use crate::OrbitRuntime;
use crate::application::job::exec::V2RunFinalizationOptions;
use crate::application::job::resume::ResumePlan;
use crate::application::operation::{
    child_admission_authority, inherit_child_admission, reserved_operation_key_error,
};

#[cfg(unix)]
use std::os::unix::process::CommandExt;
#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;

#[cfg(unix)]
use super::run::CANCELLATION_WORKER_EXIT_AUDIT;

mod admission;
mod submit;
mod wait;
mod worker;

pub(crate) use admission::input_hash;
pub(crate) use submit::{ChildPipelineAdmission, ChildSubmission};
pub(crate) use submit::{PipelineSubmission, SubmittedDefinition};
#[cfg(test)]
pub(crate) use wait::{PIPELINE_WAIT_MAX_TIMEOUT_SECONDS, PipelineWaitClock};
pub use wait::{PipelineWaitEntry, PipelineWaitResult, pipeline_wait_status_is_success};
#[cfg(test)]
pub(crate) use worker::command::{
    configure_pipeline_worker_command, pipeline_worker_profile_file, pipeline_worker_root_override,
    resolve_pipeline_worker_executable, worker_command_override, worker_observer_read_counter,
};
pub(crate) use worker::command::{run_definition_snapshot_path, workspace_auto_run_input};
#[cfg(test)]
pub(crate) use worker::log::configure_pipeline_worker_stdio;
pub(crate) use worker::log::pipeline_worker_log_path;
#[cfg(all(test, unix))]
pub(crate) use worker::log::pipeline_worker_log_test_hook;

#[derive(Debug, Clone, Serialize)]
pub struct PipelineInvokeResult {
    pub run_id: String,
    pub job_name: String,
    pub submitted_at: String,
    pub queued: bool,
}

/// [ORB-12038] A run's own `<run_id>.worker.log`, read for inspection when no
/// audited CLI-invocation blob exists to explain a terminal outcome. See
/// [`OrbitRuntime::read_pipeline_worker_log`].
#[derive(Debug, Clone, Serialize)]
pub struct PipelineWorkerLogSnapshot {
    pub path: PathBuf,
    pub content: Option<String>,
}

/// [ORB-11998] Run-input field carrying the owning workspace's `.orbit`
/// directory, as set by routine dispatch (`RuntimeDispatch::submit`). The
/// executing worker verifies its own resolved workspace against this value
/// before running any step, so a workspace-routing failure surfaces as a
/// visibly failed run instead of a silent no-op success.
pub(crate) const ROUTINE_DISPATCH_ORBIT_DIR_FIELD: &str = "__routine_dispatch_orbit_dir";

/// [ORB-12038] `error_code` recorded on the diagnostic step for a routine-
/// dispatch workspace mismatch, so `orbit run show` names the cause rather
/// than an operator finding only a bare `cancelled` state.
pub(crate) const ROUTINE_DISPATCH_WORKSPACE_MISMATCH_ERROR_CODE: &str =
    "routine_dispatch_workspace_mismatch";

/// The refusal for a submission that supplied the reserved trusted-host
/// admission key it is not entitled to write [ORB-11354].
///
/// Shared by every entry point that accepts caller-shaped run input so the
/// refusal reads identically whether it came from `orbit run job`, a direct
/// YAML path, a resume, or a tool call.
pub(crate) fn reserved_trusted_host_key_error(job_name: &str) -> OrbitError {
    OrbitError::InvalidInput(format!(
        "run input for job '{job_name}' set the reserved `{TRUSTED_HOST_ADMISSION_KEY}` field; \
         trusted host execution is admitted per invocation by the governed `orbit.agent.invoke` \
         operation and cannot be requested through ordinary job input"
    ))
}
