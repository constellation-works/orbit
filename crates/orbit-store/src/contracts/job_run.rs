//! Job-run record shapes: the outcomes of run lifecycle writes and the
//! parameters of child-run, keyed and step admissions.

use chrono::{DateTime, Utc};
use orbit_types::workflow::{JobRun, JobRunState};
use serde_json::Value;

/// Top-level run-input keys the dashboard's run list and run detail use
/// for task labels (`task_id` and `task_ids`). A steps-free row read keeps
/// only these.
pub(crate) const JOB_RUN_TASK_INPUT_KEYS: &[&str] = &["task_id", "task_ids"];

/// The authoritative result of one attempted terminal transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobRunFinalization {
    /// The run did not exist.
    Missing,
    /// This call changed a live run to its requested terminal state.
    Finalized,
    /// An earlier terminal state won; this call did not change it.
    AlreadyTerminal(JobRunState),
}

/// The result of cancelling a run only while it is still `pending`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingJobRunCancellation {
    /// The run did not exist.
    Missing,
    /// This call moved the still-pending run to `cancelled`.
    Cancelled,
    /// The run had left `pending` (it is in this state); nothing was written.
    NotPending(JobRunState),
}

/// Durable inputs for one parent-authorized child admission.
#[derive(Debug, Clone)]
pub struct ChildJobRunAdmissionParams {
    pub parent_run_id: String,
    pub parent_step_id: Option<String>,
    pub job_id: String,
    pub action: String,
    pub blocking: bool,
    pub attempt: u32,
    pub scheduled_at: DateTime<Utc>,
    pub input: Option<Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ChildJobRunAdmissionOutcome {
    Admitted(Box<JobRun>),
    AdmissionsStopped,
}

/// One keyed top-level submission: within the newest `scan_limit` runs of
/// `job_id`, a run whose input carries the same retry key is resolved rather
/// than a new one admitted.
#[derive(Debug, Clone)]
pub struct KeyedJobRunParams {
    pub job_id: String,
    /// Input field whose string value is the retry key. It must be present
    /// and non-blank so the admitted run is visible to later probes.
    pub retry_key_field: String,
    /// How many of the job's newest runs the key is matched against.
    pub scan_limit: usize,
    pub scheduled_at: DateTime<Utc>,
    pub input: Value,
}

/// Outcome of an atomic retry-key or automation-key admission.
#[derive(Debug, Clone, PartialEq)]
pub enum KeyedJobRunAdmission {
    /// No matching run carried the key; this one was inserted.
    Admitted(Box<JobRun>),
    /// The run bound to the automation key, or the newest run in the retry
    /// window carrying the key. Nothing was written.
    Existing(Box<JobRun>),
}

#[derive(Debug, Clone)]
pub struct JobRunStepParams {
    /// Lock acquisition wait in the step; unknown for historical records.
    pub lock_wait_ms: Option<u64>,

    pub step_index: usize,
    pub target_type: orbit_types::workflow::JobTargetType,
    pub target_id: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub duration_ms: Option<u64>,
    pub exit_code: Option<i32>,
    pub agent_response_json: Option<Value>,
    pub state: JobRunState,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
}
