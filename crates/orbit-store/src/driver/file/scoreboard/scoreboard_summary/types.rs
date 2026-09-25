//! Scoreboard summary document types, windows and generation inputs.

use super::{NotableCompletions, ScoreboardCoverage};
use crate::friction_store::FrictionReportedCount;
use crate::{AuditToolCallCountsByRole, AuditToolCallCountsBySurfaceAndRole, AuditTopToolCall};
use chrono::{DateTime, Duration, Utc};
use orbit_common::OrbitError;
use orbit_types::workflow::JobRun;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

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
pub(super) const CURRENT_SCHEMA_VERSION: u32 = 9;

pub const ORCHESTRATION_SCHEMA_VERSION: u32 = 2;

pub(super) const RECENT_WINDOW_DAYS: i64 = 7;

pub(super) type FamilyScoreboard = BTreeMap<String, BTreeMap<String, u64>>;

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
