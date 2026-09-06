use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::identity::OrbitId;

pub const fn default_job_max_active_runs() -> u32 {
    1
}

pub const fn default_max_iterations() -> u32 {
    1
}

pub const fn default_retry_backoff_seconds() -> u64 {
    10
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[serde(rename_all = "snake_case")]
pub enum JobTargetType {
    #[default]
    #[cfg_attr(feature = "clap", value(name = "activity", alias = "activity"))]
    Activity,
    #[cfg_attr(feature = "clap", value(name = "job", alias = "job"))]
    Job,
}

impl Display for JobTargetType {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            JobTargetType::Activity => write!(f, "activity"),
            JobTargetType::Job => write!(f, "job"),
        }
    }
}

impl FromStr for JobTargetType {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "activity" => Ok(JobTargetType::Activity),
            "job" => Ok(JobTargetType::Job),
            other => Err(format!("unknown job target type: {other}")),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[serde(rename_all = "snake_case")]
pub enum JobScheduleState {
    Enabled,
    Disabled,
}

impl Display for JobScheduleState {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            JobScheduleState::Enabled => write!(f, "enabled"),
            JobScheduleState::Disabled => write!(f, "disabled"),
        }
    }
}

impl FromStr for JobScheduleState {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "enabled" | "active" => Ok(JobScheduleState::Enabled),
            "disabled" | "deleted" | "paused" => Ok(JobScheduleState::Disabled),
            other => Err(format!("unknown job state: {other}")),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[serde(rename_all = "snake_case")]
pub enum JobRunState {
    Pending,
    Running,
    Success,
    Failed,
    Timeout,
    Skipped,
    /// Transient state emitted while the engine is sleeping between retry attempts.
    Retrying,
    /// Run was explicitly cancelled by the user before it completed.
    Cancelled,
    /// Run was orphaned: its owner process died (crash, SIGKILL, host reboot)
    /// without finalizing the run. Terminal for finalization purposes, but the
    /// run's persisted step checkpoints can seed a resumed follow-up run
    /// (ORB-10002).
    Interrupted,
}

/// [ORB-10965] Outcome of applying a `Start` event to a job run.
///
/// Scheduling is at-least-once, so the same queued run can be delivered to two
/// workers, or redelivered to the worker that already owns it. Start is
/// therefore idempotent *per owner*: exactly one caller is granted execution
/// authority, and a duplicate delivery from that same owner is a no-op rather
/// than a state-machine violation. A duplicate from a *different* owner is a
/// conflict — the incumbent keeps authority — and is reported separately so
/// the caller can distinguish it from a genuinely illegal transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobRunStartOutcome {
    /// This caller won the transition into `running` and owns execution.
    Started,
    /// The run had already been started by an equivalent owner. Nothing was
    /// written; the first Start still owns execution.
    AlreadyStarted,
    /// No run with the requested id exists.
    NotFound,
}

impl JobRunStartOutcome {
    /// True only for the single caller that won the transition. Every other
    /// outcome — including a same-owner redelivery — must not execute the run.
    pub fn owns_execution(self) -> bool {
        matches!(self, Self::Started)
    }
}

/// [ORB-11253] Outcome of a transactional pipeline-state mutation.
///
/// A run's state is one document that several writers touch: the engine
/// checkpoints steps and records child dispatches, and an operator sets run
/// controls. "Read it, change it, write it" loses whichever change landed in
/// between, and a run terminalizes on its own worker's schedule, so both the
/// decision and the write have to happen in one transaction. This is what that
/// combined operation answers; the two refusals write nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStateUpdate {
    /// The state was mutated and persisted.
    Updated,
    /// No run with the requested id exists in this workspace.
    NotFound,
    /// The run exists but has no persisted pipeline state yet.
    NoState,
}

/// Events that drive job run state transitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunEvent {
    Start,
    Complete,
    Fail,
    Timeout,
    Cancel,
    Abandon,
    /// Owner process died without finalizing the run (orphan reconciliation).
    Interrupt,
}

impl Display for RunEvent {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            RunEvent::Start => write!(f, "start"),
            RunEvent::Complete => write!(f, "complete"),
            RunEvent::Fail => write!(f, "fail"),
            RunEvent::Timeout => write!(f, "timeout"),
            RunEvent::Cancel => write!(f, "cancel"),
            RunEvent::Abandon => write!(f, "abandon"),
            RunEvent::Interrupt => write!(f, "interrupt"),
        }
    }
}

impl JobRunState {
    /// Returns true if this state cannot be overwritten by a later finalization.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Success | Self::Failed | Self::Timeout | Self::Cancelled | Self::Interrupted
        )
    }

    /// Validate and compute the next state for a given event.
    pub fn try_transition(self, event: RunEvent) -> Result<JobRunState, String> {
        // Terminal states reject all events
        if self.is_terminal() {
            return Err(format!(
                "invalid job run state transition: {} + {:?} (state is terminal)",
                self, event
            ));
        }

        match (self, event) {
            (Self::Pending, RunEvent::Start) => Ok(Self::Running),
            (Self::Pending, RunEvent::Cancel) => Ok(Self::Cancelled),
            // [ORB-10070] A queued run whose worker process died (or was never
            // claimed after a host reboot) is orphaned exactly like a running
            // run with a dead owner; reconcile finalizes both as interrupted.
            (Self::Pending, RunEvent::Interrupt) => Ok(Self::Interrupted),
            (Self::Running, RunEvent::Complete) => Ok(Self::Success),
            (Self::Running, RunEvent::Fail) => Ok(Self::Failed),
            (Self::Running, RunEvent::Timeout) => Ok(Self::Timeout),
            (Self::Running, RunEvent::Cancel) => Ok(Self::Cancelled),
            (Self::Running, RunEvent::Abandon) => Ok(Self::Failed),
            (Self::Running, RunEvent::Interrupt) => Ok(Self::Interrupted),
            _ => Err(format!(
                "invalid job run state transition: {} + {:?}",
                self, event
            )),
        }
    }

    /// Validates that a step result state is one of the allowed write-once values.
    pub fn validate_step_state(self) -> Result<(), String> {
        match self {
            Self::Success
            | Self::Failed
            | Self::Timeout
            | Self::Skipped
            | Self::Cancelled
            | Self::Interrupted => Ok(()),
            other => Err(format!(
                "invalid step result state: {} (must be success, failed, timeout, skipped, cancelled, or interrupted)",
                other
            )),
        }
    }
}

impl Display for JobRunState {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            JobRunState::Pending => write!(f, "pending"),
            JobRunState::Running => write!(f, "running"),
            JobRunState::Success => write!(f, "success"),
            JobRunState::Failed => write!(f, "failed"),
            JobRunState::Timeout => write!(f, "timeout"),
            JobRunState::Skipped => write!(f, "skipped"),
            JobRunState::Retrying => write!(f, "retrying"),
            JobRunState::Cancelled => write!(f, "cancelled"),
            JobRunState::Interrupted => write!(f, "interrupted"),
        }
    }
}

impl FromStr for JobRunState {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "pending" => Ok(JobRunState::Pending),
            "running" => Ok(JobRunState::Running),
            "success" => Ok(JobRunState::Success),
            "failed" => Ok(JobRunState::Failed),
            "timeout" => Ok(JobRunState::Timeout),
            "skipped" => Ok(JobRunState::Skipped),
            "retrying" => Ok(JobRunState::Retrying),
            "cancelled" => Ok(JobRunState::Cancelled),
            "interrupted" => Ok(JobRunState::Interrupted),
            other => Err(format!("unknown job run state: {other}")),
        }
    }
}

/// Step execution condition.
///
/// Keyword shorthands (`always`, `on_success`, `on_failure`, `on_timeout`) are
/// kept for backwards compatibility. Any other string is treated as an
/// expression that is evaluated at runtime against the template context.
///
/// Expression syntax (after template resolution):
///   `<lhs> == <rhs>`, `<lhs> != <rhs>`, combined with `&&` / `||`.
///   `&&` binds tighter than `||`.
///
/// Example:
///   `"{{steps.plan.state.status}} == success && {{steps.gate.output.match}} == true"`
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum StepCondition {
    #[default]
    Always,
    OnSuccess,
    OnFailure,
    OnTimeout,
    /// A runtime expression evaluated against the template context.
    Expr(String),
}

impl Serialize for StepCondition {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Always => serializer.serialize_str("always"),
            Self::OnSuccess => serializer.serialize_str("on_success"),
            Self::OnFailure => serializer.serialize_str("on_failure"),
            Self::OnTimeout => serializer.serialize_str("on_timeout"),
            Self::Expr(expr) => serializer.serialize_str(expr),
        }
    }
}

impl<'de> Deserialize<'de> for StepCondition {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(match s.as_str() {
            "always" => Self::Always,
            "on_success" => Self::OnSuccess,
            "on_failure" => Self::OnFailure,
            "on_timeout" => Self::OnTimeout,
            _ => Self::Expr(s),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentRunError {
    pub code: String,
    pub message: String,
    #[serde(default)]
    pub details: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentResponseEnvelope {
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    pub status: String,
    #[serde(default)]
    pub result: Option<Value>,
    #[serde(default)]
    pub error: Option<AgentRunError>,
    #[serde(default)]
    #[serde(rename = "durationMs")]
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentCommitRequest {
    pub message: String,
    pub files: Vec<String>,
}

/// A single step within a job definition.
///
/// `Default::default()` matches serde defaults for all fields:
/// - `retry_backoff_seconds` defaults to 10 (matching `#[serde(default = "default_retry_backoff_seconds")]`)
/// - `timeout_seconds` defaults to 0; callers must set this explicitly
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct JobStep {
    /// Optional step identifier for DAG execution. When any step in a job has
    /// `upstream`, ALL steps must have an `id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Steps that must complete before this step can start. Empty means no
    /// dependencies. When any step has `upstream`, DAG execution is used.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub upstream: Vec<String>,
    pub target_type: JobTargetType,
    pub target_id: OrbitId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_input: Option<Value>,
    #[serde(default)]
    pub agent_cli: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub timeout_seconds: u64,
    /// Additional env var names to pass through in hermetic mode, on top of the global allowlist.
    #[serde(default)]
    pub env_extra: Vec<String>,
    /// Explicit env var key-value pairs injected into the step's environment.
    /// Unlike `env_extra` (which passes names from the parent env), these set
    /// fixed values regardless of what the parent env contains.  Entries here
    /// override same-named vars from `env_extra` or the global allowlist.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub env_set: HashMap<String, String>,
    /// Maximum number of total attempts (including the first). Zero means no retry.
    #[serde(default)]
    pub retry_max_attempts: u32,
    /// Initial backoff delay in seconds before the first retry; doubles with each attempt.
    #[serde(default = "default_retry_backoff_seconds")]
    pub retry_backoff_seconds: u64,
    #[serde(default)]
    pub condition: StepCondition,
    /// Rename output keys before merging into the next step's input.
    /// Each entry maps `source_key -> target_key`. Unmapped keys pass through unchanged.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub output_map: HashMap<String, String>,
}

impl Default for JobStep {
    fn default() -> Self {
        Self {
            id: None,
            upstream: Vec::new(),
            target_type: JobTargetType::default(),
            target_id: OrbitId::default(),
            default_input: None,
            agent_cli: String::new(),
            executor: None,
            model: None,
            timeout_seconds: 0,
            env_extra: Vec::new(),
            env_set: HashMap::new(),
            retry_max_attempts: 0,
            retry_backoff_seconds: default_retry_backoff_seconds(),
            condition: StepCondition::Always,
            output_map: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Job {
    pub job_id: OrbitId,
    pub state: JobScheduleState,
    #[serde(default)]
    pub default_input: Option<Value>,
    #[serde(default = "default_job_max_active_runs")]
    pub max_active_runs: u32,
    /// Maximum number of times the step sequence is executed. Defaults to 1
    /// (single pass). Values > 1 enable loop semantics: after all steps
    /// complete successfully, the sequence restarts from step 0 until
    /// `max_iterations` is reached or a step outputs `loop_exit: true`.
    #[serde(default = "default_max_iterations")]
    pub max_iterations: u32,
    pub steps: Vec<JobStep>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Per-step execution record stored in a step file inside the run bundle directory.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JobRunStep {
    pub step_index: u32,
    pub target_type: JobTargetType,
    pub target_id: OrbitId,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub duration_ms: Option<u64>,
    pub exit_code: Option<i32>,
    pub agent_response_json: Option<Value>,
    pub state: JobRunState,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct KnowledgeRunMetrics {
    pub raw_read_token_baseline: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub knowledge_pack_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compression_ratio: Option<f64>,
    /// Historical read-token counter. New traces no longer increment this
    /// (ORB-10828); the field remains so stored job runs keep deserializing.
    pub actual_fs_read_tokens_during_run: u64,
    /// Historical gauge. New traces leave this unset (ORB-10828).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub double_read_rate: Option<f64>,
    pub knowledge_pack_used: bool,
    #[serde(default)]
    pub knowledge_pack_unresolved_count: u32,
    pub total_llm_input_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JobRun {
    pub run_id: OrbitId,
    pub job_id: OrbitId,
    pub attempt: u32,
    pub state: JobRunState,
    pub scheduled_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub duration_ms: Option<u64>,
    pub created_at: DateTime<Utc>,
    /// OS PID of the process that owns this run; set when the run transitions to `running`.
    /// Used to detect abandoned runs when the owning process has died.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// Process start-time token captured alongside `pid` so reused PIDs are not
    /// mistaken for the original run owner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid_start_time: Option<String>,
    /// The original input passed to this run, persisted so retries can reconstruct state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Value>,
    /// When this run is a retry, links back to the source run ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_source_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub knowledge_metrics: Option<KnowledgeRunMetrics>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_crew: Option<String>,
    #[serde(
        default,
        alias = "implementer_model",
        skip_serializing_if = "Option::is_none"
    )]
    pub crew_model: Option<String>,
    /// Step execution results; populated in-memory from step files, not stored in jrun.yaml.
    #[serde(skip)]
    pub steps: Vec<JobRunStep>,
}

impl JobRun {
    /// [ORB-10965] Whether `pid` — paired with its process start-time identity
    /// token, so a reused PID is not mistaken for the original — is the owner
    /// already recorded on this run.
    ///
    /// Owner identity is the equivalence key for a duplicate `Start`. The
    /// `started_at` instant deliberately is not: every redelivery carries a
    /// fresh timestamp, and the incumbent's value stays authoritative.
    pub fn is_owned_by(&self, pid: u32, pid_start_time: Option<&str>) -> bool {
        self.pid == Some(pid) && self.pid_start_time.as_deref() == pid_start_time
    }
}
