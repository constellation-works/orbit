//! Incident contract types: failure classes, incidents, the query and the
//! report, and the labels and limits they share.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Maximum gap between one cluster's last event and the next cluster's first
/// event for the two to be treated as one cascade within a job run. Failure
/// propagation up a pipeline's enclosing steps is effectively immediate; a
/// minute of slack absorbs step teardown without swallowing a genuinely
/// separate failure later in the same run.
pub const CASCADE_WINDOW_SECS: i64 = 60;

/// How an audit failure should be read by an operator.
///
/// The four classes are kept separate at every layer: an incident never
/// merges rows of different classes, so a policy denial, expected negative,
/// or lifecycle diagnostic can never be counted as an unexpected failure (or
/// hide one).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureClass {
    /// The call was refused by policy or capability before it ran.
    Denied,
    /// The call ran and failed on a documented negative path — invalid input,
    /// a missing record, a validation refusal. The system behaved correctly.
    Expected,
    /// An abnormal-path lifecycle record emitted for diagnosis. These rows
    /// have no healthy-call population and therefore never form a tool rate.
    Diagnostic,
    /// Everything else: a failure that is not a known negative path.
    Unexpected,
}

impl FailureClass {
    pub fn as_str(self) -> &'static str {
        match self {
            FailureClass::Denied => "denied",
            FailureClass::Expected => "expected",
            FailureClass::Diagnostic => "diagnostic",
            FailureClass::Unexpected => "unexpected",
        }
    }

    /// Operator-facing label. Rendered next to the count so "12 denied" is
    /// never mistaken for "12 things broke".
    pub fn label(self) -> &'static str {
        match self {
            FailureClass::Denied => "policy denial",
            FailureClass::Expected => "expected negative path",
            FailureClass::Diagnostic => "lifecycle diagnostic",
            FailureClass::Unexpected => "unexpected failure",
        }
    }
}

/// One raw audit row referenced by an incident. Enough to identify the exact
/// underlying evidence and jump to it in the raw Audit view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IncidentEventRef {
    pub id: i64,
    pub ts: DateTime<Utc>,
    pub execution_id: String,
    pub status: String,
    pub role: String,
    pub surface: String,
    pub run_id: Option<String>,
    pub task_id: Option<String>,
    pub activity_id: Option<String>,
    /// Tool name when the row had one; `None` for direct CLI and legacy
    /// job-run lifecycle events.
    pub tool_name: Option<String>,
    pub message: Option<String>,
}

/// One downstream link of a cascade: a distinct failure signature that
/// followed the incident's root within the same run and cascade window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PropagationLink {
    pub signature: String,
    /// Surface (tool name, or `command`/`subcommand`) that reported it.
    pub surface: String,
    /// Enclosing activity/step id when the audit row carried one.
    pub activity_id: Option<String>,
    pub event_count: u64,
    pub first_ts: DateTime<Utc>,
    pub last_ts: DateTime<Utc>,
    pub message: Option<String>,
    pub sample_events: Vec<IncidentEventRef>,
}

/// A grouped failure incident: one root cause, its raw event population, and
/// the propagation chain it triggered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailureIncident {
    /// Deterministic id derived from the grouping key. Stable across repeated
    /// aggregation of the same rows; never a database identifier.
    pub incident_id: String,
    /// Human-readable grouping key. Shown in the UI so an operator can see
    /// *why* these rows were grouped.
    pub signature: String,
    pub class: FailureClass,
    pub role: String,
    pub surface: String,
    pub activity_id: Option<String>,
    pub message: Option<String>,
    /// Raw audit rows collapsed into this incident, including its
    /// propagation chain. This is the forensic count.
    pub event_count: u64,
    /// Raw audit rows matching the root signature alone.
    pub root_event_count: u64,
    pub first_ts: DateTime<Utc>,
    pub last_ts: DateTime<Utc>,
    pub run_ids: Vec<String>,
    pub task_ids: Vec<String>,
    /// Bounded sample of the root's raw rows, newest first.
    pub sample_events: Vec<IncidentEventRef>,
    /// Every raw audit row collapsed into this incident, including the
    /// propagation chain. Unbounded so expansion can show the full evidence
    /// set; [`Self::sample_events`] stays the bounded root preview.
    pub events: Vec<IncidentEventRef>,
    /// False when the root row had no tool identity — either a direct CLI
    /// command or a job-run lifecycle failure, never a tool named `unknown`.
    pub has_tool_identity: bool,
    /// Downstream failures collapsed beneath the root, in occurrence order.
    pub propagation: Vec<PropagationLink>,
}

impl FailureIncident {
    /// Raw rows attributed to the propagation chain only.
    pub fn propagated_event_count(&self) -> u64 {
        self.event_count.saturating_sub(self.root_event_count)
    }
}

/// Window and scope for a failure-incident aggregation.
#[derive(Debug, Clone, Default)]
pub struct FailureIncidentQuery {
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    pub role: Option<String>,
    pub workspace_id: Option<String>,
    /// Cap on raw rows scanned. `0` applies [`DEFAULT_SCAN_LIMIT`].
    pub max_events: usize,
}

/// Default cap on raw failure rows scanned for one aggregation.
pub const DEFAULT_SCAN_LIMIT: usize = 20_000;

/// Category key for failed audit rows with no tool identity. These are
/// job-run / activity lifecycle events, not a synthetic tool named `unknown`.
pub const JOB_RUN_LIFECYCLE_CATEGORY: &str = "job_run_lifecycle";

/// Operator-facing label for [`JOB_RUN_LIFECYCLE_CATEGORY`].
pub const JOB_RUN_LIFECYCLE_LABEL: &str = "job-run lifecycle";

/// Lifecycle audit surfaces that are emitted only when an abnormal path
/// occurs. Unlike callable tools and direct CLI commands, they cannot have a
/// healthy-call denominator. `Start` is the legacy no-tool job-start record;
/// the remaining surfaces carry their identity in `tool_name`.
pub const FAILURE_ONLY_DIAGNOSTIC_SURFACES: &[&str] = &[
    "Start",
    "pipeline.run.terminal_conflict",
    "pipeline.worker.exit",
    "pipeline.worker.startup",
];

/// Operator-facing label for abnormal-path lifecycle records, whether they
/// use one of [`FAILURE_ONLY_DIAGNOSTIC_SURFACES`] or have no tool identity.
pub const LIFECYCLE_DIAGNOSTIC_LABEL: &str = "lifecycle diagnostics";

/// Result of one aggregation: the incidents plus the raw denominators they
/// were derived from, so no surface has to re-derive (or guess) them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailureIncidentReport {
    pub incidents: Vec<FailureIncident>,
    /// Raw failed audit rows considered (all classes).
    pub raw_failed_events: u64,
    /// Raw rows per class, keyed by [`FailureClass::as_str`].
    pub raw_events_by_class: BTreeMap<String, u64>,
    /// Incident count per class.
    pub incidents_by_class: BTreeMap<String, u64>,
    /// Unique affected run count per class.
    pub affected_runs_by_class: BTreeMap<String, u64>,
    /// Unique `job_run_id`s on the grouped incidents (the affected-run count).
    pub affected_run_count: u64,
    /// Raw failed rows with no tool identity.
    pub job_run_lifecycle_events: u64,
    /// Incidents whose root had no tool identity.
    pub job_run_lifecycle_incidents: u64,
    /// Raw diagnostic rows, including both no-tool lifecycle rows and named
    /// failure-only diagnostic surfaces.
    pub lifecycle_diagnostic_events: u64,
    /// Diagnostic incidents over the same raw population.
    pub lifecycle_diagnostic_incidents: u64,
    /// Unique runs affected by diagnostic incidents.
    pub lifecycle_diagnostic_affected_run_count: u64,
    /// True when the scan hit `max_events` and older rows were not read.
    pub truncated: bool,
}

impl FailureIncidentReport {
    pub fn incident_count(&self) -> u64 {
        self.incidents.len() as u64
    }
}
