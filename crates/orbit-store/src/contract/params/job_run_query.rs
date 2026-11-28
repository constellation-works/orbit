use chrono::{DateTime, Utc};
use orbit_types::workflow::{JobRun, JobRunState, JobTargetType};

#[derive(Debug, Clone)]
pub struct JobRunQuery {
    pub job_id: Option<String>,
    /// Exact string membership in the run's submitted `input.task_ids` array,
    /// or equality with a text top-level `input.task_id`. Applied before
    /// ordering, limiting and row/step hydration. Missing or non-array
    /// `task_ids`, non-text `task_id`, and nested fields do not match; other
    /// input fields confer no ownership.
    pub task_id: Option<String>,
    /// Exact name of the routine recorded as the run's trigger
    /// (`pipeline state.trigger.routine`), whichever trigger kind fired it.
    /// Applied before ordering and limiting; pair it with `job_id` so the
    /// scan stays inside one job's history.
    pub trigger_routine: Option<String>,
    pub state: Option<JobRunState>,
    /// Match any listed state before ordering and limiting. Empty means
    /// unrestricted; other state predicates are intersected with this set.
    pub states: Vec<JobRunState>,
    /// Whether to include only states for which `JobRunState::is_terminal()`
    /// returns true. Applied before ordering and limiting.
    pub terminal_only: bool,
    /// Whether to include only `pending` and `running` rows. Applied before
    /// ordering, limiting, and step hydration so admission and orphan scans
    /// never read terminal history [ORB-11762]. Independent of `state` and
    /// `terminal_only`; combining this with `terminal_only` yields no rows.
    pub active_only: bool,
    pub created_since: Option<DateTime<Utc>>,
    /// Inclusive lower bound on run recency: `finished_at`, else `started_at`,
    /// else `created_at`. Applied before ordering, limiting and step hydration,
    /// independently of `created_since` and `order_by`.
    pub recency_since: Option<DateTime<Utc>>,
    pub limit: Option<usize>,
    /// Which timestamp `limit` truncates against. Defaults to `CreatedAt` so
    /// existing CLI/history callers keep their current ordering.
    pub order_by: JobRunOrder,
    /// When false, listing returns run rows with empty `steps` and never
    /// selects `job_run_steps` (including `agent_response_json`). Defaults
    /// to true so CLI/history callers keep hydrated pages.
    pub include_steps: bool,
    /// When set, each run's `input` keeps only these top-level keys of a JSON
    /// object; an input that is not an object is kept whole. The other values
    /// are skipped while rows are read, so a caller that reads a few input
    /// fields across a long history never materializes whole inputs. `None`
    /// keeps every input whole.
    pub input_keys: Option<&'static [&'static str]>,
}

impl Default for JobRunQuery {
    fn default() -> Self {
        Self {
            job_id: None,
            task_id: None,
            trigger_routine: None,
            state: None,
            states: Vec::new(),
            terminal_only: false,
            active_only: false,
            created_since: None,
            recency_since: None,
            limit: None,
            order_by: JobRunOrder::default(),
            include_steps: true,
            input_keys: None,
        }
    }
}

/// Which timestamp a bounded [`JobRunQuery`] orders and truncates by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum JobRunOrder {
    /// `created_at DESC, run_id ASC` — the historical list/CLI default.
    #[default]
    CreatedAt,
    /// The same "most recent activity" timestamp displayed runs are ranked
    /// by: `finished_at`, else `started_at`, else `created_at`, each DESC
    /// with `run_id ASC` as the deterministic tiebreak.
    Recency,
}

/// Top-level string predicate on `job_runs.input_json` for a bounded
/// step-targeted run query.
#[derive(Debug, Clone)]
pub struct JobRunInputTextFilter {
    /// Top-level JSON object key name (ASCII identifier characters only).
    pub key: &'static str,
    /// Allowed string values for the key.
    pub values: Vec<String>,
    /// Whether `TRIM(...)` is applied before comparing the JSON string value.
    pub trim: bool,
    /// Whether an absent or JSON-`null` key also matches.
    pub allow_missing: bool,
}

/// Bounded run query filtered by one recorded step target (`job_run_steps`),
/// hydrating only matching steps without `agent_response_json`.
#[derive(Debug, Clone, Default)]
pub struct JobRunStepTargetQuery {
    /// Allowed `job_id` values. Empty matches every job.
    pub job_ids: Vec<String>,
    /// Optional run-level `state` filter.
    pub state: Option<JobRunState>,
    /// Inclusive lower bound on run recency (`finished_at`, else `started_at`,
    /// else `created_at`).
    pub recency_since: Option<DateTime<Utc>>,
    /// Exact `target_id` on `job_run_steps` to select and hydrate.
    pub step_target_id: String,
    /// Optional `target_type` on `job_run_steps`.
    pub step_target_type: Option<JobTargetType>,
    /// Optional exact `state` required on the matching step.
    pub step_state: Option<JobRunState>,
    /// Optional `state` excluded on the matching step (`step.state <> ?`).
    pub step_exclude_state: Option<JobRunState>,
    /// Require the matching step to carry a non-null `error_message`.
    pub step_require_error_message: bool,
    /// Optional top-level string filters on `job_runs.input_json`.
    pub input_text_filters: Vec<JobRunInputTextFilter>,
    /// Top-level `input_json` keys to hydrate (`Some(&[])` skips `input_json`).
    pub input_keys: Option<&'static [&'static str]>,
    /// Keyset cursor to resume strictly after a prior page in recency order.
    pub cursor: Option<JobRunStepTargetCursor>,
    /// Maximum matching step-run rows to return, ordered by run recency
    /// descending then `run_id` ascending.
    pub limit: Option<usize>,
}

/// Keyset cursor for [`JobRunStepTargetQuery`] pagination in recency order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobRunStepTargetCursor {
    /// Raw `COALESCE(finished_at, started_at, created_at)` column value of the last scanned row.
    pub recency_raw: String,
    /// `run_id` of the last scanned row.
    pub run_id: String,
}

/// One bounded page returned by a step-target run query.
#[derive(Debug, Clone, Default)]
pub struct JobRunStepTargetPage {
    /// Matching runs in `(recency DESC, run_id ASC)` order.
    pub runs: Vec<JobRun>,
    /// Cursor to continue strictly after this page when additional rows remain.
    pub next_cursor: Option<JobRunStepTargetCursor>,
    /// Number of matching step-run rows read for this page.
    pub scanned_rows: usize,
}
