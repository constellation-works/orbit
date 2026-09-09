use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use orbit_common::OrbitError;
use orbit_types::identity::{
    all_agent_families, infer_agent_family_from_model, normalize_attribution_label,
    normalize_optional_attribution_label,
};
use orbit_types::task::{Task, TaskStatus};
use orbit_types::workflow::{JobRun, JobRunState};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::friction_store::FrictionReportedCount;
use crate::{AuditToolCallCountsByRole, AuditToolCallCountsBySurfaceAndRole, AuditTopToolCall};
use orbit_common::fs::io::atomic_write_text_volatile as write_atomic;

mod highlights;

pub use highlights::{
    CoverageAvailability, CoverageNote, NotableCompletion, NotableCompletions, ScoreboardCoverage,
    select_notable_completions, snapshot_coverage,
};

const SUMMARY_FILENAME: &str = "summary.json";
const PR_SCOREBOARD_FILENAME: &str = "pr.json";
const TOKEN_SCOREBOARD_FILENAME: &str = "tokens.json";
// v2 adds `task_review.threads`; v3 adds tasks_created/tasks_planned,
// per-(role, surface) tool call counts, top-level workflows_run, and a
// recent_7d window block. v5 adds per-agent `friction.reported`
// (from append-only `.orbit/frictions/` records, matching `orbit.friction.stats`).
// v6 ([ORB-00337]) adds top-level `window` + `window_since` fields and the
// `ScoreboardInputs.window` plumbing — snapshot-sourced per-agent fields
// (`tokens`, `pr`, `task_review.threads`) zero out under non-`All`
// windows because we lack a timestamped snapshot log to filter against.
// v7 adds the separately-versioned `orchestration` projection. Its v2 token
// extension normalizes provider input semantics and retains model attribution
// without folding it into execution-agent/model scoreboard rows.
// v8 removes retired competition projections. Older readers ignore
// unknown fields and maintained consumers treat absent fields as empty.
// v9 ([ORB-10873]) adds `notable_completions` (priority then recency
// reading order, not a quality score) and `coverage` notes that distinguish
// observed zeros from snapshot sources that cannot be windowed.
const CURRENT_SCHEMA_VERSION: u32 = 9;
pub const ORCHESTRATION_SCHEMA_VERSION: u32 = 2;
const RECENT_WINDOW_DAYS: i64 = 7;

type FamilyScoreboard = BTreeMap<String, BTreeMap<String, u64>>;

/// Time window for a scoreboard summary. `All` is the legacy lifetime view —
/// every non-`All` variant carries a finite `duration()` used as the cutoff
/// for windowed source filtering inside [`generate_summary_with_inputs`].
///
/// String forms (used in the dashboard query param and the serialized
/// `ScoreboardSummary.window` field): `1h`, `24h`, `7d`, `30d`, `all`.
///
/// Snapshot-sourced fields (`tokens`, `pr`, `task_review.threads`)
/// have no per-event timestamp, so they zero out under any non-`All` window;
/// see the v6 schema comment. Per-(role) audit aggregates are filtered at
/// query time by the caller (the runtime in `orbit-core`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScoreboardWindow {
    Hour,
    Day,
    Week,
    Month,
    #[default]
    All,
}

impl ScoreboardWindow {
    /// Window length, or `None` for `All` (no cutoff).
    pub fn duration(self) -> Option<Duration> {
        match self {
            ScoreboardWindow::Hour => Some(Duration::hours(1)),
            ScoreboardWindow::Day => Some(Duration::hours(24)),
            ScoreboardWindow::Week => Some(Duration::days(7)),
            ScoreboardWindow::Month => Some(Duration::days(30)),
            ScoreboardWindow::All => None,
        }
    }

    /// Canonical short string used in the dashboard query param and the
    /// serialized `ScoreboardSummary.window` field.
    pub fn as_str(self) -> &'static str {
        match self {
            ScoreboardWindow::Hour => "1h",
            ScoreboardWindow::Day => "24h",
            ScoreboardWindow::Week => "7d",
            ScoreboardWindow::Month => "30d",
            ScoreboardWindow::All => "all",
        }
    }
}

impl std::str::FromStr for ScoreboardWindow {
    type Err = OrbitError;

    /// Parse a canonical window string. Accepts exactly `"1h"`, `"24h"`,
    /// `"7d"`, `"30d"`, or `"all"`. Any other input returns
    /// [`OrbitError::InvalidInput`] so HTTP callers can render an exact 400.
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "1h" => Ok(ScoreboardWindow::Hour),
            "24h" => Ok(ScoreboardWindow::Day),
            "7d" => Ok(ScoreboardWindow::Week),
            "30d" => Ok(ScoreboardWindow::Month),
            "all" => Ok(ScoreboardWindow::All),
            other => Err(OrbitError::InvalidInput(format!(
                "unknown scoreboard window '{other}' (expected one of 1h, 24h, 7d, 30d, all)"
            ))),
        }
    }
}

impl TryFrom<&str> for ScoreboardWindow {
    type Error = OrbitError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        value.parse()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenSummary {
    pub total: u64,
    pub output: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PrSummary {
    pub review_comments: u64,
    pub merged_clean: u64,
    pub merged_with_revision: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct FrictionSummary {
    /// Number of append-only friction records reported by this agent family.
    /// Sourced from `.orbit/frictions/` (via the same aggregation as
    /// `orbit.friction.stats` / `friction_stats`), not from legacy task status
    /// or `tool_calls_by_surface.friction`.
    pub reported: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentSummary {
    pub tasks_completed: u64,
    #[serde(default)]
    pub tasks_created: u64,
    #[serde(default)]
    pub tasks_planned: u64,
    pub tokens: TokenSummary,
    pub pr: PrSummary,
    #[serde(default)]
    pub friction: FrictionSummary,
    pub tool_calls: u64,
    #[serde(default)]
    pub failed_tool_calls: u64,
    /// Per-Orbit-surface tool call counts (e.g. `graph` → 56, `task` → 102).
    /// The surface key is the segment after the `orbit.` namespace prefix —
    /// see [`AuditToolCallCountsBySurfaceAndRole`].
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tool_calls_by_surface: BTreeMap<String, u64>,
}

/// Top-level "completed `orbit run` jobs" rollup. Not per-agent: a workflow
/// is a job-level concept and routinely fans out across multiple agents.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowRunCount {
    pub job_id: String,
    pub count: u64,
}

/// One row of the "most-called tools" leaderboard — `count` invocations of
/// `tool_name` attributed to `role`. Sourced from the audit log; restricted
/// to `orbit.*` tools by the SQL filter.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TopToolCall {
    pub role: String,
    pub tool_name: String,
    pub count: u64,
}

/// Headline totals over the most recent [`RECENT_WINDOW_DAYS`]. Carries no
/// per-agent breakdowns by design — the section is a "is this still being
/// used" recency signal, not a leaderboard.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecentSummary {
    /// Lower bound of the window (inclusive), RFC3339.
    pub since: String,
    pub tasks_created: u64,
    pub tasks_completed: u64,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tool_calls_by_surface: BTreeMap<String, u64>,
    pub workflows_run: u64,
}

/// Conservative ownership class for managed invocation accounting.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum OrchestrationBucketKind {
    Missing,
    Unattributed,
    Orchestrator,
    Shared,
}

/// One managed-execution accounting bucket, classified by canonical task
/// orchestration ownership rather than executor agent or model identity.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OrchestrationBucketSummary {
    pub kind: OrchestrationBucketKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub orchestrator: Option<String>,
    pub invocation_count: u64,
    pub linked_task_count: u64,
    pub input_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_create_tokens: u64,
    pub cache_create_1h_tokens: u64,
    pub output_tokens: u64,
    pub provider_cost_usd: f64,
    pub provider_cost_count: u64,
    pub derived_cost_usd: f64,
    pub derived_cost_count: u64,
    pub comparable_provider_cost_usd: f64,
    pub comparable_derived_cost_usd: f64,
    pub comparable_cost_count: u64,
    pub comparable_cost_delta_usd: f64,
    pub missing_provider_count: u64,
    pub unpriced_derived_count: u64,
    /// Normalized, mutually-exclusive token usage for covered models only.
    #[serde(default)]
    pub normalized_tokens: NormalizedTokenSummary,
    /// Model attribution is descriptive only; tokenizers are not ranked.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<OrchestrationModelSummary>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct NormalizedTokenSummary {
    pub invocation_count: u64,
    pub linked_task_count: u64,
    pub uncached_input_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_create_tokens: u64,
    pub cache_create_1h_tokens: u64,
    pub output_tokens: u64,
    pub normalized_token_total: u64,
    pub covered_invocation_count: u64,
    pub unknown_input_basis_or_model_count: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct OrchestrationModelSummary {
    pub model: String,
    pub tokens: NormalizedTokenSummary,
}

/// Independently-versioned accounting for managed execution only.
///
/// `until` is exclusive and no later than `as_of`. Provider, derived, and
/// comparable cost populations are separate so partial sums are never implied
/// to reconcile.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OrchestrationSummary {
    pub schema_version: u32,
    pub scope: String,
    pub as_of: DateTime<Utc>,
    pub since: Option<DateTime<Utc>>,
    pub until: DateTime<Utc>,
    pub buckets: Vec<OrchestrationBucketSummary>,
    #[serde(default)]
    pub normalized_tokens: NormalizedTokenSummary,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_normalized_tokens: Option<NormalizedTokenSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScoreboardSummary {
    pub schema_version: u32,
    pub generated_at: String,
    pub agents: BTreeMap<String, AgentSummary>,
    /// Top jobs by completed-run count, descending. Empty when the runtime
    /// passed no JobRun records (e.g. backward-compat callers).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workflows_run: Vec<WorkflowRunCount>,
    /// Top (role, tool_name) pairs across the audit log, restricted to
    /// `orbit.*` tool names. Already sorted desc by count.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub top_tools: Vec<TopToolCall>,
    /// Recency window for headline deltas on the public scoreboard. The
    /// 7d boundary here is independent of the user-selected scoreboard
    /// `window` — `recent_7d` is a "is this still being used" signal,
    /// always over the same fixed period, not a leaderboard.
    /// Optional so older readers / unit tests that don't wire it tolerate
    /// its absence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recent_7d: Option<RecentSummary>,
    /// Managed-execution accounting by task orchestrator, deliberately kept
    /// outside executor-agent rankings. v7+.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orchestration: Option<OrchestrationSummary>,
    /// Selected scoreboard window in canonical short form (`"1h"`,
    /// `"24h"`, `"7d"`, `"30d"`, `"all"`). v6+. Older readers tolerate
    /// the field's absence via `#[serde(default)]`.
    #[serde(default)]
    pub window: String,
    /// RFC3339 lower bound of the scoreboard window, or `None` when the
    /// window is `All` (lifetime). v6+.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_since: Option<String>,
    /// Compact delivery highlights for the selected window. v9+.
    #[serde(default)]
    pub notable_completions: NotableCompletions,
    /// Distinguishes observed empty sections from sources that cannot be
    /// attributed to a finite window. v9+.
    #[serde(default)]
    pub coverage: ScoreboardCoverage,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct TokenScoreboardFile {
    #[serde(default)]
    agents: Vec<TokenAgentEntry>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct TokenAgentEntry {
    #[serde(rename = "agent")]
    _agent: String,
    /// Model key used for this token scoreboard row; per-invocation actual execution (from audit/token metrics, not run-level lineup).
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    total_tokens: u64,
    #[serde(default, alias = "output_tokens")]
    total_output_tokens: u64,
    #[serde(default)]
    total_tool_calls: u64,
}

/// Bundle of the optional inputs that have grown around the core task summary.
/// New callers should populate this struct;
/// the older `generate_summary*` thin wrappers stay for tests and any
/// caller that hasn't been updated yet.
#[derive(Debug, Clone)]
pub struct ScoreboardInputs<'a> {
    /// Per-(role) tool-call totals — drives the legacy `tool_calls`/
    /// `failed_tool_calls` columns.
    pub audit_tool_calls: &'a [AuditToolCallCountsByRole],
    /// Per-(role, surface) tool-call counts. All-time.
    pub audit_tool_calls_by_surface: &'a [AuditToolCallCountsBySurfaceAndRole],
    /// Per-(role, surface) tool-call counts windowed to the most recent
    /// [`RECENT_WINDOW_DAYS`]. Drives the `recent_7d.tool_calls_by_surface`
    /// totals.
    pub audit_tool_calls_by_surface_recent: &'a [AuditToolCallCountsBySurfaceAndRole],
    /// All persisted JobRun records — successful ones populate the
    /// `workflows_run` rollup; the lot drives the 7d workflows count.
    pub job_runs: &'a [JobRun],
    /// Top (role, tool_name) pairs across the audit log, sorted desc by
    /// count. Drives the "most-called tools" leaderboard.
    pub top_tool_calls: &'a [AuditTopToolCall],
    /// Friction counts per reporting model label, already windowed by the
    /// caller with the same cutoff this module derives from `now`/`window`.
    /// Populates per-family `friction.reported` counts (so the dashboard
    /// friction column and `orbit.friction.stats` agree, without using tool
    /// call surface counts). ORB-10680 replaced the full record slice with
    /// this aggregate so scoreboard memory tracks distinct models, not corpus
    /// size.
    pub friction_reported: &'a [FrictionReportedCount],
    /// Reference "now" for recency windowing. `None` means no recency
    /// section is emitted (used by legacy callers).
    pub now: Option<DateTime<Utc>>,
    /// User-selected scoreboard window. `All` (the default) keeps the
    /// historical lifetime view. Non-`All` variants zero out snapshot-
    /// sourced fields and filter timestamp-bearing slices to the window.
    /// See [`ScoreboardWindow`] for the per-source semantics. v6+.
    pub window: ScoreboardWindow,
    /// Runtime/API-sourced accounting for the same bounded window. It does
    /// not use all-time snapshot token aggregates.
    pub orchestration: Option<OrchestrationSummary>,
}

impl<'a> Default for ScoreboardInputs<'a> {
    fn default() -> Self {
        static EMPTY_AUDIT: [AuditToolCallCountsByRole; 0] = [];
        static EMPTY_SURFACE: [AuditToolCallCountsBySurfaceAndRole; 0] = [];
        static EMPTY_JOB: [JobRun; 0] = [];
        static EMPTY_TOP: [AuditTopToolCall; 0] = [];
        Self {
            audit_tool_calls: &EMPTY_AUDIT,
            audit_tool_calls_by_surface: &EMPTY_SURFACE,
            audit_tool_calls_by_surface_recent: &EMPTY_SURFACE,
            job_runs: &EMPTY_JOB,
            top_tool_calls: &EMPTY_TOP,
            friction_reported: &[],
            now: None,
            window: ScoreboardWindow::All,
            orchestration: None,
        }
    }
}

pub fn generate_summary(
    scoreboard_dir: &Path,
    tasks: &[Task],
) -> Result<ScoreboardSummary, OrbitError> {
    generate_summary_with_inputs(scoreboard_dir, tasks, &ScoreboardInputs::default())
}

pub fn generate_summary_with_audit_tool_calls(
    scoreboard_dir: &Path,
    tasks: &[Task],
    audit_tool_calls: &[AuditToolCallCountsByRole],
) -> Result<ScoreboardSummary, OrbitError> {
    generate_summary_with_inputs(
        scoreboard_dir,
        tasks,
        &ScoreboardInputs {
            audit_tool_calls,
            ..ScoreboardInputs::default()
        },
    )
}

pub fn generate_summary_with_inputs(
    scoreboard_dir: &Path,
    tasks: &[Task],
    inputs: &ScoreboardInputs<'_>,
) -> Result<ScoreboardSummary, OrbitError> {
    let audit_tool_calls = inputs.audit_tool_calls;
    let mut agents: BTreeMap<String, AgentSummary> = BTreeMap::new();
    seed_known_family_agents(&mut agents);

    // Window cutoff. `None` (i.e. `window == All`) preserves the legacy
    // lifetime behavior; `Some(since)` triggers per-source filtering and
    // skips snapshot reads (which lack per-event timestamps).
    let now_for_window = inputs.now.unwrap_or_else(Utc::now);
    let since: Option<DateTime<Utc>> = inputs.window.duration().map(|d| now_for_window - d);
    let windowed = since.is_some();

    // Snapshot reads (pr.json, task_review.json, tokens.json) have no per-event timestamp, so they only run
    // for the lifetime (`All`) window. Under a windowed view they zero
    // out — the frontend renders 0 as `—` via emptyScoreboardNode().
    // TODO(phase-3+): timestamped snapshot logs would unblock real
    // windowing of these columns.
    if !windowed {
        let pr = read_model_scoreboard(scoreboard_dir)?;
        overlay_nested_metric(&mut agents, &pr, "pr-review-comments", |summary, value| {
            summary.pr.review_comments = summary.pr.review_comments.saturating_add(value);
        });
        overlay_nested_metric(
            &mut agents,
            &pr,
            "pr-count-without-revision",
            |summary, value| {
                summary.pr.merged_clean = summary.pr.merged_clean.saturating_add(value);
            },
        );
        overlay_nested_metric(
            &mut agents,
            &pr,
            "pr-count-with-revision",
            |summary, value| {
                summary.pr.merged_with_revision =
                    summary.pr.merged_with_revision.saturating_add(value);
            },
        );

        for token_row in read_token_agents(scoreboard_dir)? {
            let Some(model) = token_row
                .model
                .as_deref()
                .map(family_key)
                .filter(|value| !value.is_empty())
            else {
                continue;
            };
            let summary = agents.entry(model).or_default();
            summary.tokens.total = summary.tokens.total.saturating_add(token_row.total_tokens);
            summary.tokens.output = summary
                .tokens
                .output
                .saturating_add(token_row.total_output_tokens);
            summary.tool_calls = summary
                .tool_calls
                .saturating_add(token_row.total_tool_calls);
        }
    }

    overlay_audit_tool_calls(&mut agents, audit_tool_calls);
    overlay_audit_tool_calls_by_surface(&mut agents, inputs.audit_tool_calls_by_surface);

    overlay_friction_reported(&mut agents, inputs.friction_reported);

    for task in tasks {
        if matches!(task.status, TaskStatus::Done | TaskStatus::Archived)
            && in_window(task_done_at(task), since)
            && let Some(model) = normalize_optional_attribution_label(
                task.implemented_by.as_deref(),
                task.implemented_by.as_deref(),
            )
        {
            let summary = agents.entry(family_key(&model)).or_default();
            summary.tasks_completed = summary.tasks_completed.saturating_add(1);
        }

        // Created/Planned count *all* statuses — see [T20260508-16]: rejected
        // tasks still represent real work the agent produced.
        // Windowed views filter by `created_at` so an old task created
        // outside the window doesn't get re-counted today.
        if in_window(Some(task.created_at), since) {
            if let Some(label) = task
                .created_by
                .as_deref()
                .map(|raw| normalize_attribution_label(raw, None))
                .filter(|value| !value.is_empty())
            {
                let summary = agents.entry(family_key(&label)).or_default();
                summary.tasks_created = summary.tasks_created.saturating_add(1);
            }
            if let Some(label) = task
                .planned_by
                .as_deref()
                .map(|raw| normalize_attribution_label(raw, None))
                .filter(|value| !value.is_empty())
            {
                let summary = agents.entry(family_key(&label)).or_default();
                summary.tasks_planned = summary.tasks_planned.saturating_add(1);
            }
        }
    }

    let workflows_run = aggregate_workflows_run(inputs.job_runs, since);
    let top_tools: Vec<TopToolCall> = inputs
        .top_tool_calls
        .iter()
        .map(|row| TopToolCall {
            role: row.role.clone(),
            tool_name: row.tool_name.clone(),
            count: row.total,
        })
        .collect();
    // recent_7d intentionally uses the full (unfiltered) tasks slice —
    // its 7d boundary is a fixed "is this still being used" signal,
    // independent of the user-selected `window`.
    let recent_7d = inputs
        .now
        .map(|now| build_recent_summary(now, tasks, inputs));
    Ok(ScoreboardSummary {
        schema_version: CURRENT_SCHEMA_VERSION,
        generated_at: Utc::now().to_rfc3339(),
        agents,
        workflows_run,
        top_tools,
        recent_7d,
        orchestration: inputs.orchestration.clone(),
        window: inputs.window.as_str().to_string(),
        window_since: since.map(|t| t.to_rfc3339()),
        notable_completions: select_notable_completions(tasks, since),
        coverage: snapshot_coverage(windowed),
    })
}

/// `true` when `timestamp` is at or after `since`. `since == None` means
/// the lifetime window — everything is in-window.
fn in_window(timestamp: Option<DateTime<Utc>>, since: Option<DateTime<Utc>>) -> bool {
    match (timestamp, since) {
        (_, None) => true,
        (Some(ts), Some(cut)) => ts >= cut,
        (None, Some(_)) => false,
    }
}

fn aggregate_workflows_run(runs: &[JobRun], since: Option<DateTime<Utc>>) -> Vec<WorkflowRunCount> {
    let mut counts: BTreeMap<String, u64> = BTreeMap::new();
    for run in runs {
        if run.state == JobRunState::Success && in_window(Some(run_completed_at(run)), since) {
            *counts.entry(run.job_id.to_string()).or_insert(0) += 1;
        }
    }
    let mut rows: Vec<WorkflowRunCount> = counts
        .into_iter()
        .map(|(job_id, count)| WorkflowRunCount { job_id, count })
        .collect();
    // Highest run-count first; tie-break by job_id ASC for stable output.
    rows.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.job_id.cmp(&b.job_id)));
    rows
}

fn build_recent_summary(
    now: DateTime<Utc>,
    tasks: &[Task],
    inputs: &ScoreboardInputs<'_>,
) -> RecentSummary {
    let since = now - Duration::days(RECENT_WINDOW_DAYS);

    let mut tasks_created: u64 = 0;
    let mut tasks_completed: u64 = 0;
    for task in tasks {
        if task.created_at >= since {
            tasks_created = tasks_created.saturating_add(1);
        }
        if matches!(task.status, TaskStatus::Done | TaskStatus::Archived)
            && task_done_at(task).is_some_and(|done_at| done_at >= since)
        {
            tasks_completed = tasks_completed.saturating_add(1);
        }
    }

    let mut tool_calls_by_surface: BTreeMap<String, u64> = BTreeMap::new();
    for row in inputs.audit_tool_calls_by_surface_recent {
        *tool_calls_by_surface
            .entry(row.surface.clone())
            .or_insert(0) += row.total;
    }

    let workflows_run: u64 = inputs
        .job_runs
        .iter()
        .filter(|run| run.state == JobRunState::Success)
        .filter(|run| run_completed_at(run) >= since)
        .count() as u64;

    RecentSummary {
        since: since.to_rfc3339(),
        tasks_created,
        tasks_completed,
        tool_calls_by_surface,
        workflows_run,
    }
}

/// Best-effort timestamp for when a task entered `done`/`archived`.
/// Task history is no longer embedded in the public task DTO, so summary
/// generation uses the envelope `updated_at` timestamp.
fn task_done_at(task: &Task) -> Option<DateTime<Utc>> {
    Some(task.updated_at)
}

/// Best-effort completion timestamp for a JobRun. `finished_at` is set when
/// the run terminates; the fallback to `created_at` keeps the recency
/// filter conservative for legacy rows that pre-date that field.
fn run_completed_at(run: &JobRun) -> DateTime<Utc> {
    run.finished_at.unwrap_or(run.created_at)
}

pub fn write_summary(
    scoreboard_dir: &Path,
    summary: &ScoreboardSummary,
) -> Result<std::path::PathBuf, OrbitError> {
    let path = scoreboard_dir.join(SUMMARY_FILENAME);
    let raw = serde_json::to_string_pretty(summary)
        .map_err(|e| OrbitError::Io(format!("serialize summary.json: {e}")))?;
    write_atomic(&path, &format!("{raw}\n"))?;
    Ok(path)
}

pub fn summary_path(scoreboard_dir: &Path) -> std::path::PathBuf {
    scoreboard_dir.join(SUMMARY_FILENAME)
}

fn read_model_scoreboard(scoreboard_dir: &Path) -> Result<FamilyScoreboard, OrbitError> {
    let Some(path) = validated_scoreboard_file_path(scoreboard_dir, ScoreboardFile::Pr)? else {
        return Ok(FamilyScoreboard::new());
    };
    let raw = fs::read_to_string(&path)
        .map_err(|e| OrbitError::Io(format!("read {PR_SCOREBOARD_FILENAME}: {e}")))?;
    if raw.trim().is_empty() {
        return Ok(FamilyScoreboard::new());
    }
    let parsed: Value = serde_json::from_str(&raw)
        .map_err(|e| OrbitError::Io(format!("parse {PR_SCOREBOARD_FILENAME}: {e}")))?;
    normalize_model_scoreboard(parsed)
}

/// The fixed snapshot files are selected by the store, never by a caller.
#[derive(Clone, Copy)]
enum ScoreboardFile {
    Pr,
    Tokens,
}

impl ScoreboardFile {
    fn filename(self) -> &'static str {
        match self {
            Self::Pr => PR_SCOREBOARD_FILENAME,
            Self::Tokens => TOKEN_SCOREBOARD_FILENAME,
        }
    }
}

/// Resolve a fixed scoreboard file beneath the selected scoreboard root.
///
/// The root is selected by the workspace configuration, but the file read by
/// this summary is fixed. Canonicalizing the root, checking containment, and
/// rejecting a symlink or non-regular target prevents a path component from
/// redirecting this read to an unrelated file.
fn validated_scoreboard_file_path(
    scoreboard_dir: &Path,
    file: ScoreboardFile,
) -> Result<Option<PathBuf>, OrbitError> {
    let canonical_dir = match fs::canonicalize(scoreboard_dir) {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(OrbitError::Io(format!(
                "canonicalize scoreboard directory {}: {error}",
                scoreboard_dir.display()
            )));
        }
    };
    if !canonical_dir.is_dir() {
        return Err(OrbitError::InvalidInput(format!(
            "scoreboard path must be a directory: {}",
            scoreboard_dir.display()
        )));
    }

    let path = canonical_dir.join(file.filename());
    if !path.starts_with(&canonical_dir) {
        return Err(OrbitError::InvalidInput(format!(
            "scoreboard file must remain within the scoreboard directory: {}",
            path.display()
        )));
    }

    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(OrbitError::InvalidInput(
            format!("scoreboard file must not be a symlink: {}", path.display()),
        )),
        Ok(metadata) if !metadata.is_file() => Err(OrbitError::InvalidInput(format!(
            "scoreboard file must be a regular file: {}",
            path.display()
        ))),
        Ok(_) => Ok(Some(path)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(OrbitError::Io(format!(
            "inspect scoreboard file {}: {error}",
            path.display()
        ))),
    }
}

fn read_token_agents(scoreboard_dir: &Path) -> Result<Vec<TokenAgentEntry>, OrbitError> {
    let Some(path) = validated_scoreboard_file_path(scoreboard_dir, ScoreboardFile::Tokens)? else {
        return Ok(Vec::new());
    };
    let raw =
        fs::read_to_string(&path).map_err(|e| OrbitError::Io(format!("read tokens.json: {e}")))?;
    if raw.trim().is_empty() {
        return Ok(Vec::new());
    }
    let parsed: TokenScoreboardFile = serde_json::from_str(&raw)
        .map_err(|e| OrbitError::Io(format!("parse tokens.json: {e}")))?;
    Ok(parsed.agents)
}

fn overlay_nested_metric(
    agents: &mut BTreeMap<String, AgentSummary>,
    scoreboard: &FamilyScoreboard,
    metric: &str,
    mut apply: impl FnMut(&mut AgentSummary, u64),
) {
    let Some(by_family) = scoreboard.get(metric) else {
        return;
    };

    for (family, value) in by_family {
        let summary = agents.entry(family_key(family)).or_default();
        apply(summary, *value);
    }
}

fn overlay_audit_tool_calls_by_surface(
    agents: &mut BTreeMap<String, AgentSummary>,
    rows: &[AuditToolCallCountsBySurfaceAndRole],
) {
    for row in rows {
        let family = family_key(&row.role);
        if family.is_empty() {
            continue;
        }
        let summary = agents.entry(family).or_default();
        let entry = summary
            .tool_calls_by_surface
            .entry(row.surface.clone())
            .or_insert(0);
        *entry = entry.saturating_add(row.total);
    }
}

fn overlay_audit_tool_calls(
    agents: &mut BTreeMap<String, AgentSummary>,
    audit_tool_calls: &[AuditToolCallCountsByRole],
) {
    let mut by_family: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    for row in audit_tool_calls {
        let family = family_key(&row.role);
        if family.is_empty() {
            continue;
        }
        let entry = by_family.entry(family).or_default();
        entry.0 = entry.0.saturating_add(row.total);
        entry.1 = entry.1.saturating_add(row.failed);
    }

    for (family, (total, failed)) in by_family {
        let summary = agents.entry(family).or_default();
        // Total competes with token scoreboard data; failures only exist in audit rows.
        summary.tool_calls = summary.tool_calls.max(total);
        summary.failed_tool_calls = summary.failed_tool_calls.saturating_add(failed);
    }
}

/// Fold per-model friction counts into per-family agent rows. The caller has
/// already applied the window cutoff in SQL, so this only maps model labels to
/// families and sums collisions.
fn overlay_friction_reported(
    agents: &mut BTreeMap<String, AgentSummary>,
    reported: &[FrictionReportedCount],
) {
    let mut counts: BTreeMap<String, u64> = BTreeMap::new();
    for entry in reported {
        let family = {
            let normalized =
                normalize_optional_attribution_label(Some(&entry.model), None).unwrap_or_default();
            infer_agent_family_from_model(&normalized).unwrap_or(normalized)
        };
        *counts.entry(family).or_insert(0) += entry.count;
    }
    for (family, count) in counts {
        let summary = agents.entry(family).or_default();
        summary.friction.reported = count;
    }
}

fn family_key(label: &str) -> String {
    let normalized = normalize_attribution_label(label, None);
    infer_agent_family_from_model(&normalized).unwrap_or(normalized)
}

fn seed_known_family_agents(agents: &mut BTreeMap<String, AgentSummary>) {
    for family in all_agent_families() {
        agents.entry(family.to_string()).or_default();
    }
}

fn normalize_model_scoreboard(parsed: Value) -> Result<FamilyScoreboard, OrbitError> {
    let mut normalized = FamilyScoreboard::new();
    let Value::Object(metrics) = parsed else {
        return Err(OrbitError::Io(
            "scoreboard json must be an object".to_string(),
        ));
    };

    for (metric, metric_value) in metrics {
        let Value::Object(entries) = metric_value else {
            continue;
        };
        let family_entries = normalized.entry(metric).or_default();
        for (first_key, first_value) in entries {
            match first_value {
                Value::Number(number) => {
                    let value = number.as_u64().ok_or_else(|| {
                        OrbitError::Io("scoreboard counter must be u64".to_string())
                    })?;
                    *family_entries.entry(family_key(&first_key)).or_insert(0) += value;
                }
                Value::Object(inner) => {
                    for (family, value) in inner {
                        let Value::Number(number) = value else {
                            continue;
                        };
                        let count = number.as_u64().ok_or_else(|| {
                            OrbitError::Io("scoreboard counter must be u64".to_string())
                        })?;
                        *family_entries.entry(family_key(&family)).or_insert(0) += count;
                    }
                }
                _ => {}
            }
        }
    }

    Ok(normalized)
}

#[cfg(test)]
mod tests;
