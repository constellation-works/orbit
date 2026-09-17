//! Failure-incident grouping over raw audit events [ORB-10871].
//!
//! A raw failed audit event is forensic evidence, not an incident. One burst
//! of the same refusal repeated hundreds of times is one problem, and one
//! failed pipeline run that propagates its failure up through its enclosing
//! steps is one root cause with a chain hanging off it. Counting the raw rows
//! as independent failures overstates both.
//!
//! This module derives an incident view *on top of* the audit rows without
//! consuming or rewriting them: every incident carries its raw `event_count`
//! and the ids of the rows it collapsed, so the raw Audit view and any export
//! stay the authority on evidence.
//!
//! The contract is deliberately project-agnostic — it reads only durable
//! audit columns (status, role, tool/command, error message, run/task ids,
//! timestamps). No tool name, agent, workspace, or task id is special-cased.
//!
//! Grouping runs in three passes:
//!
//! 1. **Classify** each failed row as [`FailureClass::Denied`],
//!    [`FailureClass::Expected`], [`FailureClass::Diagnostic`], or
//!    [`FailureClass::Unexpected`] so a policy refusal, caller/input negative,
//!    and lifecycle diagnostic stay distinguishable from a genuine unexpected
//!    failure.
//! 2. **Cluster** rows by `(run scope, signature)`, where the signature is
//!    `class | role | surface | normalized message`. Volatile tokens (paths,
//!    numbers, ids, timestamps, hashes, quoted literals) are replaced with
//!    placeholders, so a repeated failure whose only difference is its operand
//!    collapses into one cluster.
//! 3. **Collapse cascades** within a single job run: clusters of the same
//!    class whose time ranges are within [`CASCADE_WINDOW_SECS`] of each other
//!    are one incident, rooted at the earliest cluster, with the later ones
//!    recorded as its propagation chain. A failure later in the same run,
//!    beyond that window, stays an independent incident.
//! 4. **Collapse cited-run cascades** across job runs: a later incident whose
//!    raw message names another incident's `job_run_id` (parent/child guard
//!    copies) folds onto that cited root. Matching is token ∩ known run ids,
//!    not a special-cased tool or activity name.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Duration, Utc};
use orbit_types::telemetry::{AuditEvent, AuditEventStatus};
use serde::{Deserialize, Serialize};

/// Maximum gap between one cluster's last event and the next cluster's first
/// event for the two to be treated as one cascade within a job run. Failure
/// propagation up a pipeline's enclosing steps is effectively immediate; a
/// minute of slack absorbs step teardown without swallowing a genuinely
/// separate failure later in the same run.
pub const CASCADE_WINDOW_SECS: i64 = 60;

/// Per-incident cap on the sampled raw events echoed back to callers. The
/// full population is always reachable through the raw Audit view; this bound
/// keeps one burst of thousands from dominating a response body.
const MAX_SAMPLE_EVENTS: usize = 20;

/// Longest normalized message retained in a signature. Long enough to keep
/// distinct failures distinct, short enough that a multi-kilobyte error body
/// cannot become a grouping key.
const MAX_SIGNATURE_MESSAGE_CHARS: usize = 160;

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

/// Message fragments that mark a documented negative path. Each mirrors an
/// `OrbitError` variant's `Display` rendering in `orbit-common`, which every
/// surface funnels its errors through before they reach `error_message`.
/// Matching is lowercase substring, so a wrapped/prefixed message still
/// classifies.
const EXPECTED_FAILURE_MARKERS: &[&str] = &[
    "invalid input",
    "not found",
    "already exists",
    "validation failed",
    "sensitive input rejected",
    "invalid status transition",
    "unsupported",
    "not local to the current worktree",
    "artifact unavailable",
    "companion not installed",
];

/// Message fragments that mark a refusal rather than a failure. A refusal
/// recorded with `status = failure` (some surfaces translate late) still
/// classifies as a denial so the two populations do not blur.
const DENIAL_MARKERS: &[&str] = &["policy denied", "capability denied", "permission denied"];

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

/// Groups already-filtered failure rows into a report. Split out from the
/// store call so the contract is testable without a database.
pub fn build_report(failures: &[AuditEvent], truncated: bool) -> FailureIncidentReport {
    let incidents = group_failure_incidents(failures);

    let mut raw_events_by_class: BTreeMap<String, u64> = BTreeMap::new();
    let mut job_run_lifecycle_events: u64 = 0;
    for event in failures {
        *raw_events_by_class
            .entry(classify(event).as_str().to_string())
            .or_insert(0) += 1;
        if !has_tool_identity(event) {
            job_run_lifecycle_events += 1;
        }
    }
    let mut incidents_by_class: BTreeMap<String, u64> = BTreeMap::new();
    let mut run_ids: BTreeSet<String> = BTreeSet::new();
    let mut run_ids_by_class: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut job_run_lifecycle_incidents: u64 = 0;
    for incident in &incidents {
        *incidents_by_class
            .entry(incident.class.as_str().to_string())
            .or_insert(0) += 1;
        if !incident.has_tool_identity {
            job_run_lifecycle_incidents += 1;
        }
        for run_id in &incident.run_ids {
            run_ids.insert(run_id.clone());
            run_ids_by_class
                .entry(incident.class.as_str().to_string())
                .or_default()
                .insert(run_id.clone());
        }
    }
    let affected_runs_by_class: BTreeMap<String, u64> = run_ids_by_class
        .into_iter()
        .map(|(class, ids)| (class, ids.len() as u64))
        .collect();
    let lifecycle_diagnostic_events = raw_events_by_class
        .get(FailureClass::Diagnostic.as_str())
        .copied()
        .unwrap_or(0);
    let lifecycle_diagnostic_incidents = incidents_by_class
        .get(FailureClass::Diagnostic.as_str())
        .copied()
        .unwrap_or(0);
    let lifecycle_diagnostic_affected_run_count = affected_runs_by_class
        .get(FailureClass::Diagnostic.as_str())
        .copied()
        .unwrap_or(0);

    FailureIncidentReport {
        raw_failed_events: failures.len() as u64,
        raw_events_by_class,
        incidents_by_class,
        affected_runs_by_class,
        incidents,
        affected_run_count: run_ids.len() as u64,
        job_run_lifecycle_events,
        job_run_lifecycle_incidents,
        lifecycle_diagnostic_events,
        lifecycle_diagnostic_incidents,
        lifecycle_diagnostic_affected_run_count,
        truncated,
    }
}

/// Classifies one failed audit row. Denial status wins outright; otherwise the
/// `OrbitError`-derived markers decide whether this was a documented negative
/// path. Anything unmatched is treated as unexpected — the conservative
/// direction, since under-reporting a real failure is the costlier mistake.
pub fn classify(event: &AuditEvent) -> FailureClass {
    if is_lifecycle_diagnostic(event) {
        return FailureClass::Diagnostic;
    }
    if matches!(event.status, AuditEventStatus::Denied) {
        return FailureClass::Denied;
    }
    let message = event
        .error_message
        .as_deref()
        .unwrap_or_default()
        .to_lowercase();
    if message.is_empty() {
        return FailureClass::Unexpected;
    }
    if DENIAL_MARKERS.iter().any(|marker| message.contains(marker)) {
        return FailureClass::Denied;
    }
    if EXPECTED_FAILURE_MARKERS
        .iter()
        .any(|marker| message.contains(marker))
    {
        return FailureClass::Expected;
    }
    FailureClass::Unexpected
}

/// True for a failure-only diagnostic surface. Callers use the same predicate
/// when excluding named rows from callable-tool rate rankings.
pub fn is_failure_only_diagnostic_surface(name: &str) -> bool {
    let name = name.trim();
    FAILURE_ONLY_DIAGNOSTIC_SURFACES.contains(&name)
}

/// True when an audit row is a lifecycle diagnostic rather than a call whose
/// success and failure populations can be compared.
pub fn is_lifecycle_diagnostic(event: &AuditEvent) -> bool {
    is_failure_only_diagnostic_surface(&surface_of(event))
}

/// True when the row names a real tool. Direct CLI and legacy job-run
/// lifecycle rows have empty/`NULL` tool names; neither is a tool called
/// `unknown`.
pub fn has_tool_identity(event: &AuditEvent) -> bool {
    event
        .tool_name
        .as_deref()
        .is_some_and(|name| !name.trim().is_empty())
}

/// The reporting surface of an audit row: its tool name when it has one,
/// otherwise its `command`/`subcommand` pair. Never empty.
fn surface_of(event: &AuditEvent) -> String {
    if let Some(tool) = event.tool_name.as_deref().filter(|name| !name.is_empty()) {
        return tool.to_string();
    }
    match event.subcommand.as_deref().filter(|sub| !sub.is_empty()) {
        Some(sub) if !event.command.is_empty() => format!("{} {sub}", event.command),
        Some(sub) => sub.to_string(),
        None if !event.command.is_empty() => event.command.clone(),
        None => "unknown".to_string(),
    }
}

/// Replaces volatile tokens in an error message so that the same failure over
/// different operands collapses to one signature.
///
/// The rules are shape-based only — no vocabulary from any project, tool, or
/// agent appears here.
pub fn normalize_message(raw: &str) -> String {
    let mut out = String::new();
    for token in raw.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&normalize_token(token));
        if out.chars().count() >= MAX_SIGNATURE_MESSAGE_CHARS {
            break;
        }
    }
    if out.chars().count() > MAX_SIGNATURE_MESSAGE_CHARS {
        out = out.chars().take(MAX_SIGNATURE_MESSAGE_CHARS).collect();
    }
    out
}

fn normalize_token(token: &str) -> Cow<'_, str> {
    let trimmed = token.trim_matches(['"', '\'', '`', '(', ')', ',']);
    if trimmed.is_empty() {
        return Cow::Borrowed(token);
    }
    // Order matters: the most specific shapes are tested first so a token that
    // satisfies several rules gets its most informative placeholder.
    if looks_like_timestamp(trimmed) {
        return Cow::Borrowed("<ts>");
    }
    if looks_like_uuid(trimmed) {
        return Cow::Borrowed("<uuid>");
    }
    if looks_like_prefixed_id(trimmed) {
        return Cow::Borrowed("<id>");
    }
    if looks_like_path(trimmed) {
        return Cow::Borrowed("<path>");
    }
    if looks_like_hex(trimmed) {
        return Cow::Borrowed("<hex>");
    }
    if looks_like_number(trimmed) {
        return Cow::Borrowed("<num>");
    }
    Cow::Owned(trimmed.to_string())
}

/// `2026-08-15T06:34:00Z` and friends: leading four digits, a `-`, and a
/// digit-dominated body.
fn looks_like_timestamp(token: &str) -> bool {
    let bytes = token.as_bytes();
    bytes.len() >= 10
        && bytes[..4].iter().all(u8::is_ascii_digit)
        && bytes[4] == b'-'
        && token.chars().filter(char::is_ascii_digit).count() >= 8
}

fn looks_like_uuid(token: &str) -> bool {
    let groups: Vec<&str> = token.split('-').collect();
    groups.len() == 5
        && [8_usize, 4, 4, 4, 12]
            .iter()
            .zip(&groups)
            .all(|(len, group)| group.len() == *len && group.chars().all(|c| c.is_ascii_hexdigit()))
}

/// `ABC-1234`, `T20260428-7`, `jrun-20260816-0634-8`: an alphanumeric stem
/// joined to a digit run. Covers generated record ids of any project without
/// naming one.
fn looks_like_prefixed_id(token: &str) -> bool {
    let Some((head, tail)) = token.split_once('-') else {
        return false;
    };
    if head.is_empty() || !head.chars().all(|c| c.is_ascii_alphanumeric()) {
        return false;
    }
    if !head.chars().any(|c| c.is_ascii_alphabetic()) {
        return false;
    }
    let rest: String = tail.chars().filter(|c| *c != '-').collect();
    !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit())
}

fn looks_like_path(token: &str) -> bool {
    token.contains('/') || token.starts_with('~') || token.contains('\\')
}

fn looks_like_hex(token: &str) -> bool {
    token.len() >= 8
        && token.chars().all(|c| c.is_ascii_hexdigit())
        && token.chars().any(|c| c.is_ascii_alphabetic())
}

fn looks_like_number(token: &str) -> bool {
    let stripped = token.trim_end_matches(['.', ':', ';']);
    !stripped.is_empty()
        && stripped
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '.' | ',' | '_' | '-' | '+'))
        && stripped.chars().any(|c| c.is_ascii_digit())
}

/// The grouping signature for one failed row: class, actor, surface, and the
/// normalized message. Rendered as a readable string so the UI can show the
/// operator exactly what was collapsed.
pub fn signature_for(event: &AuditEvent) -> String {
    let class = classify(event);
    let message = normalize_message(event.error_message.as_deref().unwrap_or_default());
    format!(
        "{}|role={}|surface={}|msg={}",
        class.as_str(),
        if event.role.is_empty() {
            "unknown"
        } else {
            event.role.as_str()
        },
        surface_of(event),
        if message.is_empty() {
            format!("exit={}", event.exit_code)
        } else {
            message
        }
    )
}

/// Deterministic 64-bit FNV-1a over the grouping key, rendered as hex. Used
/// only as a stable client-side handle for an incident — never persisted, and
/// never a security boundary.
fn incident_id(run_scope: &str, signature: &str, first_ts: DateTime<Utc>) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let key = format!(
        "{run_scope}\u{1f}{signature}\u{1f}{}",
        first_ts.to_rfc3339()
    );
    for byte in key.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("inc-{hash:016x}")
}

/// One `(run scope, signature)` bucket before cascade collapsing.
#[derive(Debug, Clone)]
struct Cluster {
    signature: String,
    class: FailureClass,
    role: String,
    surface: String,
    activity_id: Option<String>,
    message: Option<String>,
    event_count: u64,
    first_ts: DateTime<Utc>,
    last_ts: DateTime<Utc>,
    run_ids: Vec<String>,
    task_ids: Vec<String>,
    sample_events: Vec<IncidentEventRef>,
    events: Vec<IncidentEventRef>,
    has_tool_identity: bool,
}

impl Cluster {
    fn absorb(&mut self, event: &AuditEvent) {
        self.event_count += 1;
        self.first_ts = self.first_ts.min(event.timestamp);
        self.last_ts = self.last_ts.max(event.timestamp);
        if self.activity_id.is_none() {
            self.activity_id.clone_from(&event.activity_id);
        }
        if self.message.is_none() {
            self.message.clone_from(&event.error_message);
        }
        push_unique(&mut self.run_ids, event.job_run_id.as_deref());
        push_unique(&mut self.task_ids, event.task_id.as_deref());
        let reference = event_ref(event);
        self.events.push(reference.clone());
        if self.sample_events.len() < MAX_SAMPLE_EVENTS {
            self.sample_events.push(reference);
        }
        self.has_tool_identity |= has_tool_identity(event);
    }

    fn into_link(mut self) -> PropagationLink {
        sort_samples(&mut self.sample_events);
        PropagationLink {
            signature: self.signature,
            surface: self.surface,
            activity_id: self.activity_id,
            event_count: self.event_count,
            first_ts: self.first_ts,
            last_ts: self.last_ts,
            message: self.message,
            sample_events: self.sample_events,
        }
    }
}

fn push_unique(target: &mut Vec<String>, value: Option<&str>) {
    let Some(value) = value.filter(|value| !value.is_empty()) else {
        return;
    };
    if !target.iter().any(|existing| existing == value) {
        target.push(value.to_string());
    }
}

fn event_ref(event: &AuditEvent) -> IncidentEventRef {
    IncidentEventRef {
        id: event.id,
        ts: event.timestamp,
        execution_id: event.execution_id.clone(),
        status: event.status.to_string(),
        role: event.role.clone(),
        surface: surface_of(event),
        run_id: event.job_run_id.clone(),
        task_id: event.task_id.clone(),
        activity_id: event.activity_id.clone(),
        tool_name: event
            .tool_name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(ToOwned::to_owned),
        message: event.error_message.clone(),
    }
}

/// Newest raw evidence first, ties broken by id so a page is stable.
fn sort_samples(samples: &mut [IncidentEventRef]) {
    samples.sort_by(|a, b| b.ts.cmp(&a.ts).then_with(|| b.id.cmp(&a.id)));
}

/// Groups failed audit rows into incidents. Pure and order-independent: the
/// same rows in any input order produce the same incidents.
pub fn group_failure_incidents(failures: &[AuditEvent]) -> Vec<FailureIncident> {
    // Pass 1+2 — cluster by (run scope, signature).
    let mut clusters: BTreeMap<(String, String), Cluster> = BTreeMap::new();
    for event in failures {
        let signature = signature_for(event);
        let run_scope = event
            .job_run_id
            .clone()
            .filter(|run| !run.is_empty())
            .unwrap_or_default();
        clusters
            .entry((run_scope, signature.clone()))
            .and_modify(|cluster| cluster.absorb(event))
            .or_insert_with(|| {
                let mut cluster = Cluster {
                    signature,
                    class: classify(event),
                    role: event.role.clone(),
                    surface: surface_of(event),
                    activity_id: None,
                    message: None,
                    event_count: 0,
                    first_ts: event.timestamp,
                    last_ts: event.timestamp,
                    run_ids: Vec::new(),
                    task_ids: Vec::new(),
                    sample_events: Vec::new(),
                    events: Vec::new(),
                    has_tool_identity: false,
                };
                cluster.absorb(event);
                cluster
            });
    }

    // Pass 3 — collapse same-run, same-class cascades onto their earliest
    // cluster. Clusters with no run id cannot be attributed to a pipeline, so
    // each stays its own incident (still deduplicated by signature above).
    let mut by_scope: BTreeMap<String, Vec<Cluster>> = BTreeMap::new();
    let mut incidents = Vec::new();
    for ((run_scope, _), cluster) in clusters {
        if run_scope.is_empty() {
            incidents.push(incident_from(&run_scope, cluster, Vec::new()));
        } else {
            by_scope.entry(run_scope).or_default().push(cluster);
        }
    }

    let cascade_window = Duration::seconds(CASCADE_WINDOW_SECS);
    for (run_scope, scope_clusters) in by_scope {
        for (_, mut class_clusters) in partition_by_class(scope_clusters) {
            class_clusters.sort_by(|a, b| {
                a.first_ts
                    .cmp(&b.first_ts)
                    .then_with(|| a.signature.cmp(&b.signature))
            });
            let mut iter = class_clusters.into_iter();
            let Some(mut root) = iter.next() else {
                continue;
            };
            let mut chain: Vec<Cluster> = Vec::new();
            let mut chain_end = root.last_ts;
            for cluster in iter {
                if cluster.first_ts - chain_end <= cascade_window {
                    chain_end = chain_end.max(cluster.last_ts);
                    chain.push(cluster);
                    continue;
                }
                incidents.push(incident_from(&run_scope, root, std::mem::take(&mut chain)));
                chain_end = cluster.last_ts;
                root = cluster;
            }
            incidents.push(incident_from(&run_scope, root, chain));
        }
    }

    // Pass 4 — collapse parent/child propagation across job runs. A later
    // incident whose raw message cites another incident's `job_run_id` is the
    // same root cause (a guard copying a leaf failure), not a second incident.
    incidents = collapse_cited_run_cascades(incidents);

    // Newest incident first; the deterministic id breaks ties so two incidents
    // sharing a last-seen timestamp never swap places between renders.
    incidents.sort_by(|a, b| {
        b.last_ts
            .cmp(&a.last_ts)
            .then_with(|| a.incident_id.cmp(&b.incident_id))
    });
    incidents
}

fn partition_by_class(clusters: Vec<Cluster>) -> BTreeMap<FailureClass, Vec<Cluster>> {
    let mut out: BTreeMap<FailureClass, Vec<Cluster>> = BTreeMap::new();
    for cluster in clusters {
        out.entry(cluster.class).or_default().push(cluster);
    }
    out
}

fn incident_from(run_scope: &str, root: Cluster, chain: Vec<Cluster>) -> FailureIncident {
    let mut root = root;
    sort_samples(&mut root.sample_events);
    sort_samples(&mut root.events);
    let mut event_count = root.event_count;
    let mut last_ts = root.last_ts;
    let mut run_ids = root.run_ids.clone();
    let mut task_ids = root.task_ids.clone();
    let mut events = root.events.clone();
    let has_tool_identity = root.has_tool_identity;

    let mut propagation: Vec<PropagationLink> = Vec::with_capacity(chain.len());
    for cluster in chain {
        event_count += cluster.event_count;
        last_ts = last_ts.max(cluster.last_ts);
        for run_id in &cluster.run_ids {
            push_unique(&mut run_ids, Some(run_id));
        }
        for task_id in &cluster.task_ids {
            push_unique(&mut task_ids, Some(task_id));
        }
        events.extend(cluster.events.iter().cloned());
        propagation.push(cluster.into_link());
    }
    propagation.sort_by(|a, b| {
        a.first_ts
            .cmp(&b.first_ts)
            .then_with(|| a.signature.cmp(&b.signature))
    });
    sort_samples(&mut events);

    FailureIncident {
        incident_id: incident_id(run_scope, &root.signature, root.first_ts),
        signature: root.signature,
        class: root.class,
        role: root.role,
        surface: root.surface,
        activity_id: root.activity_id,
        message: root.message,
        event_count,
        root_event_count: root.event_count,
        first_ts: root.first_ts,
        last_ts,
        run_ids,
        task_ids,
        sample_events: root.sample_events,
        events,
        has_tool_identity,
        propagation,
    }
}

/// Fold incidents whose raw messages cite another incident's `job_run_id`
/// onto that cited incident. Parent/child pipeline guards are the motivating
/// case; matching is by durable columns only (message tokens ∩ known run ids).
fn collapse_cited_run_cascades(mut incidents: Vec<FailureIncident>) -> Vec<FailureIncident> {
    if incidents.len() < 2 {
        return incidents;
    }
    let cascade_window = Duration::seconds(CASCADE_WINDOW_SECS);
    loop {
        let known_runs = unique_run_index(&incidents);
        if known_runs.is_empty() {
            break;
        }
        let known_ids: BTreeMap<String, ()> = known_runs
            .keys()
            .cloned()
            .map(|run_id| (run_id, ()))
            .collect();
        let mut merge: Option<(usize, usize)> = None;
        for (from_idx, incident) in incidents.iter().enumerate() {
            for cited in cited_known_run_ids_from_incident(incident, &known_ids) {
                let Some(&onto_idx) = known_runs.get(&cited) else {
                    continue;
                };
                if onto_idx == usize::MAX || onto_idx == from_idx {
                    continue;
                }
                let onto = &incidents[onto_idx];
                if onto.class != incident.class {
                    continue;
                }
                if incident.first_ts < onto.first_ts {
                    continue;
                }
                if incident.first_ts - onto.last_ts > cascade_window {
                    continue;
                }
                merge = Some((from_idx, onto_idx));
                break;
            }
            if merge.is_some() {
                break;
            }
        }
        let Some((from_idx, onto_idx)) = merge else {
            break;
        };
        let from = incidents.remove(from_idx);
        let onto_idx = if onto_idx > from_idx {
            onto_idx - 1
        } else {
            onto_idx
        };
        merge_incident_into(&mut incidents[onto_idx], from);
    }
    incidents
}

fn unique_run_index(incidents: &[FailureIncident]) -> BTreeMap<String, usize> {
    let mut known_runs: BTreeMap<String, usize> = BTreeMap::new();
    for (idx, incident) in incidents.iter().enumerate() {
        for run_id in &incident.run_ids {
            known_runs
                .entry(run_id.clone())
                .and_modify(|existing| *existing = usize::MAX)
                .or_insert(idx);
        }
    }
    known_runs
}

fn cited_known_run_ids_from_incident(
    incident: &FailureIncident,
    known: &BTreeMap<String, ()>,
) -> Vec<String> {
    let mut found = Vec::new();
    let mut consider = |message: Option<&str>| {
        if let Some(message) = message {
            for run_id in cited_known_run_ids(message, known) {
                if !found.iter().any(|existing| existing == &run_id) {
                    found.push(run_id);
                }
            }
        }
    };
    consider(incident.message.as_deref());
    for event in incident.events.iter().chain(incident.sample_events.iter()) {
        consider(event.message.as_deref());
    }
    for link in &incident.propagation {
        consider(link.message.as_deref());
        for event in &link.sample_events {
            consider(event.message.as_deref());
        }
    }
    found
}

fn cited_known_run_ids(message: &str, known: &BTreeMap<String, ()>) -> Vec<String> {
    if known.is_empty() {
        return Vec::new();
    }
    let mut found = Vec::new();
    for token in message.split_whitespace() {
        let trimmed = token.trim_matches(['"', '\'', '`', '(', ')', ',', ':', ';']);
        if known.contains_key(trimmed) && !found.iter().any(|existing| existing == trimmed) {
            found.push(trimmed.to_string());
        }
    }
    found
}

fn merge_incident_into(root: &mut FailureIncident, child: FailureIncident) {
    root.event_count += child.event_count;
    root.last_ts = root.last_ts.max(child.last_ts);
    for run_id in &child.run_ids {
        push_unique(&mut root.run_ids, Some(run_id));
    }
    for task_id in &child.task_ids {
        push_unique(&mut root.task_ids, Some(task_id));
    }
    root.events.extend(child.events.iter().cloned());
    sort_samples(&mut root.events);
    let child_root_last = child
        .sample_events
        .iter()
        .map(|event| event.ts)
        .max()
        .unwrap_or(child.last_ts);
    root.propagation.push(PropagationLink {
        signature: child.signature,
        surface: child.surface,
        activity_id: child.activity_id,
        event_count: child.root_event_count,
        first_ts: child.first_ts,
        last_ts: child_root_last,
        message: child.message,
        sample_events: child.sample_events,
    });
    root.propagation.extend(child.propagation);
    root.propagation.sort_by(|a, b| {
        a.first_ts
            .cmp(&b.first_ts)
            .then_with(|| a.signature.cmp(&b.signature))
    });
}
