//! Clock-aligned trend windows and cohort scaffolding shared by the
//! Analytics › Metrics series.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use axum::response::{IntoResponse, Json, Response};
use chrono::{DateTime, Duration, Utc};
use orbit_core::{OrbitError, OrbitRuntime};
use orbit_types::task::{TaskStatus, UNSET_BUCKET, complexity_bucket_ord};
use serde_json::{Value, json};

use super::super::{DiagnosticsQuery, map_runtime_error};
use super::metrics::DiagnosticsRange;
use crate::runtime_memo::DASHBOARD_POLL_MEMO_TTL;
use crate::state::DashboardState;

/// Complexities a request may name; `unassessed` is an alias of the unset
/// bucket.
const COMPLEXITIES: [&str; 5] = ["low", "medium", "hard", "xhard", UNSET_BUCKET];
/// Series drawn even when the window holds none of their tasks.
const STANDING_SERIES: [&str; 4] = ["low", "medium", "hard", "xhard"];
/// A point averaging fewer tasks than this is marked thin.
pub(super) const MIN_TASKS: usize = 3;
/// Tags one request may split on.
const MAX_TAGS: usize = 8;
/// Tags listed for the picker, most used first.
const AVAILABLE_TAGS: usize = 200;
/// How far before the oldest task's filing the invocation read starts.
const READ_SLACK: Duration = Duration::days(1);

/// Bucket widths a series may use, narrowest first: 5m, 1h, 6h, 1d, 7d.
const BUCKET_LADDER_SECS: [i64; 5] = [300, 3_600, 21_600, 86_400, 604_800];
/// A window takes the narrowest width that splits it into fewer buckets.
/// The floored first bucket can add one partial bucket, so an accepted
/// response carries at most `MAX_BUCKETS + 1` buckets. This ceiling is
/// independent of the caller's date span: an explicit `since`/`month`
/// window no ladder width can split this finely is refused, and an
/// `all` window widens whole weeks past the ladder until it fits.
const MAX_BUCKETS: i64 = 60;
/// Weekly buckets start on Mondays: 1970-01-05 is four days after the epoch.
const WEEK_OFFSET_SECS: i64 = 4 * 86_400;
/// Widest ladder width, used to widen `all` windows in whole weeks.
const WEEK_SECS: i64 = 604_800;
/// Length of the trailing sum a "per day" view draws at each bucket.
const DAY_SECS: i64 = 86_400;

/// Window a trend request without `since` or `month` covers.
const DEFAULT_SINCE: &str = "24h";

/// `since` or `month` as the other diagnostics read them, else the last day.
pub(super) fn trend_range(
    q: &DiagnosticsQuery,
) -> Result<DiagnosticsRange, orbit_core::OrbitError> {
    let range = if q.since.is_some() || q.month.is_some() {
        DiagnosticsRange::from_query(q, false)?
    } else {
        DiagnosticsRange::from_query(
            &DiagnosticsQuery {
                outcome: None,
                since: Some(DEFAULT_SINCE.to_string()),
                month: None,
                limit: None,
            },
            false,
        )?
    };
    validate_trend_range(&range)?;
    Ok(range)
}

/// Refuse an explicit window the bucket ladder cannot bound, or whose
/// equally long previous window is not representable. `all` windows have
/// no previous window and are widened in [`TrendWindow::new`] instead.
fn validate_trend_range(range: &DiagnosticsRange) -> Result<(), orbit_core::OrbitError> {
    let Some(since) = range.since else {
        return Ok(());
    };
    let until = range.until.min(Utc::now());
    let open = since.min(until);
    let span = (until - open).num_seconds().max(1);
    let Some(bucket_secs) = BUCKET_LADDER_SECS
        .iter()
        .copied()
        .find(|width| span / width < MAX_BUCKETS)
    else {
        return Err(orbit_core::OrbitError::InvalidInput(format!(
            "trend window '{}' is too large: it would exceed {MAX_BUCKETS} buckets",
            range.key
        )));
    };
    let start = floor(open, bucket_secs);
    if start.checked_sub_signed(until - start).is_none() {
        return Err(orbit_core::OrbitError::InvalidInput(format!(
            "trend window '{}' is too large: its previous window is not representable",
            range.key
        )));
    }
    Ok(())
}

/// Narrowest ladder width splitting `span` into fewer than
/// [`MAX_BUCKETS`] buckets, or a whole-week multiple past the ladder
/// that does so for an `all` window's unbounded span.
fn bucket_secs_for_span(span: i64) -> i64 {
    if let Some(width) = BUCKET_LADDER_SECS
        .iter()
        .copied()
        .find(|width| span / width < MAX_BUCKETS)
    {
        return width;
    }
    let weeks = span / (MAX_BUCKETS * WEEK_SECS) + 1;
    weeks.saturating_mul(WEEK_SECS)
}

/// The buckets a series is cut into, with the equally long window before it.
///
/// The requested `since` is floored to the bucket width, so every bucket but
/// the last is complete and starts on the clock (`:00`, `00:00 UTC`, …). The
/// last bucket is the one `until` falls in.
pub(super) struct TrendWindow {
    key: String,
    pub(super) start: DateTime<Utc>,
    pub(super) until: DateTime<Utc>,
    bucket_secs: i64,
    /// `[start - (until - start), start)`; `None` for an `all` window.
    pub(super) previous: Option<(DateTime<Utc>, DateTime<Utc>)>,
    /// Buckets of history before `start` that a trailing 24h sum at the
    /// first bucket needs; zero when buckets are a day or wider.
    lead: usize,
    /// An `all` range: no bound, so nothing can be ruled out of the window.
    unbounded: bool,
}

impl TrendWindow {
    /// An `all` range (no `since`) opens at `earliest`, the oldest sample the
    /// caller read, or at `until` when there is none.
    pub(super) fn new(range: &DiagnosticsRange, earliest: Option<DateTime<Utc>>) -> Self {
        Self::build(range, earliest, false)
    }

    /// [`Self::new`] plus the lead buckets a trailing-24h sum needs at the
    /// first bucket, for the count series that offer a per-day view.
    pub(super) fn with_day_lead(range: &DiagnosticsRange, earliest: Option<DateTime<Utc>>) -> Self {
        Self::build(range, earliest, true)
    }

    fn build(range: &DiagnosticsRange, earliest: Option<DateTime<Utc>>, day_lead: bool) -> Self {
        let until = range.until.min(Utc::now());
        let open = range.since.or(earliest).unwrap_or(until).min(until);
        let span = (until - open).num_seconds().max(1);
        let bucket_secs = bucket_secs_for_span(span);
        let start = floor(open, bucket_secs);
        // Checked: an explicit window whose previous window is not
        // representable is refused by `trend_range` before this runs;
        // never panic if a caller reaches here without that admission.
        let previous = range.since.and_then(|_| {
            start
                .checked_sub_signed(until - start)
                .map(|previous_start| (previous_start, start))
        });
        let lead = if day_lead && bucket_secs < DAY_SECS && DAY_SECS % bucket_secs == 0 {
            usize::try_from(DAY_SECS / bucket_secs - 1).unwrap_or(0)
        } else {
            0
        };
        Self {
            key: range.key.clone(),
            start,
            until,
            bucket_secs,
            previous,
            lead,
            unbounded: range.since.is_none(),
        }
    }

    /// The oldest instant a caller must read to fill this window and the
    /// previous one; `None` reads everything.
    pub(super) fn read_since(range: &DiagnosticsRange) -> Option<DateTime<Utc>> {
        range.since.map(|_| {
            let window = Self::new(range, None);
            window.previous.map_or(window.start, |(start, _)| start)
        })
    }

    /// [`Self::read_since`] reaching back far enough to fill the lead buckets.
    pub(super) fn read_since_with_day_lead(range: &DiagnosticsRange) -> Option<DateTime<Utc>> {
        range.since.map(|_| {
            let window = Self::with_day_lead(range, None);
            let oldest = window.previous.map_or(window.start, |(start, _)| start);
            oldest.min(window.lead_start())
        })
    }

    /// Buckets of history before the window that [`Self::lead_bucket_of`]
    /// places rows in, oldest first.
    pub(super) fn lead_count(&self) -> usize {
        self.lead
    }

    /// Where the lead buckets open: `lead_count` bucket widths before `start`.
    fn lead_start(&self) -> DateTime<Utc> {
        let offset = Duration::seconds(self.bucket_secs.saturating_mul(self.lead as i64));
        self.start.checked_sub_signed(offset).unwrap_or(self.start)
    }

    /// The lead bucket `at` falls in, or `None` outside `[lead start, start)`.
    pub(super) fn lead_bucket_of(&self, at: DateTime<Utc>) -> Option<usize> {
        if self.lead == 0 || at < self.lead_start() || at >= self.start {
            return None;
        }
        let index = (at - self.lead_start()).num_seconds() / self.bucket_secs;
        usize::try_from(index)
            .ok()
            .map(|index| index.min(self.lead - 1))
    }

    pub(super) fn bucket_count(&self) -> usize {
        let last = floor(self.until, self.bucket_secs);
        usize::try_from((last - self.start).num_seconds() / self.bucket_secs + 1).unwrap_or(1)
    }

    /// The bucket `at` falls in, or `None` outside `[start, until]`.
    pub(super) fn bucket_of(&self, at: DateTime<Utc>) -> Option<usize> {
        if at < self.start || at > self.until {
            return None;
        }
        let index = (at - self.start).num_seconds() / self.bucket_secs;
        usize::try_from(index)
            .ok()
            .map(|index| index.min(self.bucket_count() - 1))
    }

    pub(super) fn in_previous(&self, at: DateTime<Utc>) -> bool {
        self.previous
            .is_some_and(|(start, end)| at >= start && at < end)
    }

    /// Whether an undated terminal task last written at `last_write` could
    /// have closed in this window. A completion is never newer than the
    /// task's last write, so only a write before the window rules it out.
    pub(super) fn may_hold_undated(&self, last_write: DateTime<Utc>) -> bool {
        self.unbounded || last_write >= self.start
    }

    /// [`Self::may_hold_undated`] for the previous window.
    pub(super) fn previous_may_hold_undated(&self, last_write: DateTime<Utc>) -> bool {
        self.previous.is_some_and(|(start, _)| last_write >= start)
    }

    pub(super) fn bucket_start(&self, index: usize) -> DateTime<Utc> {
        let offset = Duration::seconds(self.bucket_secs.saturating_mul(index as i64));
        self.start.checked_add_signed(offset).unwrap_or(self.start)
    }

    pub(super) fn to_json(&self) -> Value {
        json!({
            "key": self.key,
            "since": self.start.to_rfc3339(),
            "until": self.until.to_rfc3339(),
            "bucket_seconds": self.bucket_secs,
            "lead_buckets": self.lead,
            "previous_since": self.previous.map(|(start, _)| start.to_rfc3339()),
            "previous_until": self.previous.map(|(_, end)| end.to_rfc3339()),
        })
    }
}

fn floor(at: DateTime<Utc>, width: i64) -> DateTime<Utc> {
    // Whole-week widths (the ladder's widest and its widened multiples)
    // start on Mondays; narrower widths start on the epoch clock.
    let offset = if width % WEEK_SECS == 0 {
        WEEK_OFFSET_SECS
    } else {
        0
    };
    let floored = (at.timestamp() - offset).div_euclid(width) * width + offset;
    DateTime::from_timestamp(floored, 0).unwrap_or(at)
}

/// Split a comma-separated query parameter into trimmed non-empty items.
pub(super) fn comma_list(raw: Option<&str>) -> Vec<&str> {
    raw.unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .collect()
}

/// Trim an optional single-value query filter, returning `None` when empty.
pub(super) fn trimmed_non_empty(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// Parse a comma-separated `complexity` query parameter, mapping `unassessed`
/// to [`UNSET_BUCKET`].
pub(super) fn parse_complexities(
    raw: Option<&str>,
) -> Result<Option<BTreeSet<&'static str>>, String> {
    let requested = comma_list(raw);
    if requested.is_empty() {
        return Ok(None);
    }
    let mut set = BTreeSet::new();
    for value in requested {
        let bucket = if value == "unassessed" {
            UNSET_BUCKET
        } else {
            value
        };
        let Some(known) = COMPLEXITIES.iter().find(|known| **known == bucket) else {
            return Err(format!(
                "unknown complexity '{value}': expected low, medium, hard, xhard or unassessed"
            ));
        };
        set.insert(*known);
    }
    Ok(Some(set))
}

/// Shared outcome, complexity and tag cohort filters for metrics trend series.
pub(super) struct CohortFilters {
    pub(super) done_only: bool,
    /// `None` draws every complexity present.
    pub(super) complexities: Option<BTreeSet<&'static str>>,
    pub(super) tags: Vec<String>,
}

impl CohortFilters {
    pub(super) fn parse(
        outcome: Option<&str>,
        complexity: Option<&str>,
        tags: Option<&str>,
    ) -> Result<Self, OrbitError> {
        let done_only = match outcome.unwrap_or("done") {
            "done" => true,
            "all" => false,
            other => {
                return Err(OrbitError::InvalidInput(format!(
                    "unknown outcome '{other}': expected done or all"
                )));
            }
        };
        let complexities = parse_complexities(complexity).map_err(OrbitError::InvalidInput)?;
        let mut tags: Vec<String> = comma_list(tags).into_iter().map(str::to_string).collect();
        tags.sort();
        tags.dedup();
        if tags.len() > MAX_TAGS {
            return Err(OrbitError::InvalidInput(format!(
                "too many tags: at most {MAX_TAGS} may be compared"
            )));
        }
        Ok(Self {
            done_only,
            complexities,
            tags,
        })
    }

    pub(super) fn outcome_label(&self) -> &'static str {
        if self.done_only { "done" } else { "all" }
    }

    pub(super) fn complexities_key(&self) -> String {
        self.complexities
            .as_ref()
            .map(|set| set.iter().copied().collect::<Vec<_>>().join(","))
            .unwrap_or_default()
    }

    pub(super) fn in_outcome(&self, status: TaskStatus) -> bool {
        !self.done_only || status == TaskStatus::Done
    }

    pub(super) fn wants_complexity(&self, complexity: &str) -> bool {
        self.complexities
            .as_ref()
            .is_none_or(|selected| selected.contains(complexity))
    }

    pub(super) fn cohorts(&self) -> &'static [&'static str] {
        if self.tags.is_empty() {
            &["all"]
        } else {
            &["with", "without"]
        }
    }

    pub(super) fn cohort_of(&self, task_tags: &[String]) -> &'static str {
        if self.tags.is_empty() {
            "all"
        } else if task_tags.iter().any(|tag| self.tags.contains(tag)) {
            "with"
        } else {
            "without"
        }
    }
}

/// Resolve the trend range, key and memoised response for a diagnostics trend
/// endpoint.
pub(super) async fn memoized_trend_response<F, KF, CF>(
    state: &DashboardState,
    runtime: Arc<OrbitRuntime>,
    endpoint: &'static str,
    span: (Option<String>, Option<String>),
    filters: F,
    build_key: KF,
    compute: CF,
) -> Response
where
    F: Send + Sync + 'static,
    KF: FnOnce(&DiagnosticsRange, &F) -> Result<String, serde_json::Error>,
    CF: FnOnce(&OrbitRuntime, &DiagnosticsRange, &F) -> Result<Value, OrbitError> + Send + 'static,
{
    let range = match trend_range(&DiagnosticsQuery {
        outcome: None,
        since: span.0,
        month: span.1,
        limit: None,
    }) {
        Ok(range) => range,
        Err(error) => return map_runtime_error(error),
    };
    let key_payload = match build_key(&range, &filters) {
        Ok(payload) => payload,
        Err(error) => {
            return map_runtime_error(OrbitError::Execution(format!(
                "failed to serialize {endpoint} memo key: {error}"
            )));
        }
    };
    let key = (endpoint.to_string(), key_payload);
    let compute_runtime = Arc::clone(&runtime);
    match state
        .metrics_trends_memo()
        .get_or_compute(&runtime, key, DASHBOARD_POLL_MEMO_TTL, move || {
            compute(&compute_runtime, &range, &filters)
        })
        .await
    {
        Ok(served) => (served.age_header(), Json((*served.body).clone())).into_response(),
        Err(error) => map_runtime_error(error),
    }
}

/// Mean and median of `values`, or null when there are none. An even median
/// is the mean of the two central values.
pub(super) fn average(values: impl Iterator<Item = u64>) -> (Value, Value) {
    let mut values: Vec<u64> = values.collect();
    let n = values.len();
    if n == 0 {
        return (Value::Null, Value::Null);
    }
    let sum: u128 = values.iter().map(|value| u128::from(*value)).sum();
    values.sort_unstable();
    let median = if n % 2 == 1 {
        values[n / 2] as f64
    } else {
        (values[n / 2 - 1] as f64 + values[n / 2] as f64) / 2.0
    };
    (json!(sum as f64 / n as f64), json!(median))
}

/// One bucket or series-total cell in a cohort trend series.
pub(super) trait CohortCell: Default + Clone {
    fn is_empty(&self) -> bool;
    fn to_json(&self) -> Value;
}

/// Bucketed points, optional lead points, and total cell for one
/// `(complexity, cohort)` series.
pub(super) struct CohortSeries<C> {
    pub(super) points: Vec<C>,
    pub(super) lead_points: Vec<C>,
    pub(super) total: C,
}

impl<C: CohortCell> CohortSeries<C> {
    fn new(buckets: usize, lead: usize) -> Self {
        Self {
            points: vec![C::default(); buckets],
            lead_points: vec![C::default(); lead],
            total: C::default(),
        }
    }
}

/// Standing and observed `(complexity, cohort)` series for a trend window.
pub(super) struct CohortSeriesMap<C> {
    series: BTreeMap<(String, &'static str), CohortSeries<C>>,
    bucket_count: usize,
    lead_count: usize,
}

impl<C: CohortCell> CohortSeriesMap<C> {
    pub(super) fn new(filters: &CohortFilters, window: &TrendWindow) -> Self {
        let bucket_count = window.bucket_count();
        let lead_count = window.lead_count();
        let cohorts = filters.cohorts();
        let mut series = BTreeMap::new();
        for complexity in STANDING_SERIES
            .iter()
            .filter(|complexity| filters.wants_complexity(complexity))
        {
            for cohort in cohorts {
                series.insert(
                    ((*complexity).to_string(), *cohort),
                    CohortSeries::new(bucket_count, lead_count),
                );
            }
        }
        Self {
            series,
            bucket_count,
            lead_count,
        }
    }

    pub(super) fn entry_mut(
        &mut self,
        complexity: &str,
        cohort: &'static str,
    ) -> &mut CohortSeries<C> {
        self.series
            .entry((complexity.to_string(), cohort))
            .or_insert_with(|| CohortSeries::new(self.bucket_count, self.lead_count))
    }

    pub(super) fn into_json(self, include_lead: bool) -> Vec<Value> {
        let mut series: Vec<_> = self.series.into_iter().collect();
        series.sort_by(|((left, left_cohort), _), ((right, right_cohort), _)| {
            complexity_bucket_ord(left)
                .cmp(&complexity_bucket_ord(right))
                .then_with(|| left_cohort.cmp(right_cohort))
        });
        series
            .into_iter()
            .filter(|((complexity, _), series)| {
                STANDING_SERIES.contains(&complexity.as_str()) || !series.total.is_empty()
            })
            .map(|((complexity, cohort), series)| {
                let points: Vec<Value> = series.points.iter().map(CohortCell::to_json).collect();
                let total = series.total.to_json();
                if include_lead {
                    json!({
                        "complexity": complexity,
                        "cohort": cohort,
                        "points": points,
                        "lead_points": series.lead_points.iter().map(CohortCell::to_json).collect::<Vec<_>>(),
                        "total": total,
                    })
                } else {
                    json!({
                        "complexity": complexity,
                        "cohort": cohort,
                        "points": points,
                        "total": total,
                    })
                }
            })
            .collect()
    }
}

/// Fields of a terminal task needed to assign cohorts and read invocations.
pub(super) struct CohortTask {
    pub(super) task_id: String,
    pub(super) complexity: String,
    pub(super) tags: Vec<String>,
    pub(super) created_at: Option<DateTime<Utc>>,
}

/// Terminal tasks partitioned into the trend window (and optional lead buckets)
/// after applying outcome and complexity filters.
pub(super) struct CohortWindowData {
    pub(super) window: TrendWindow,
    pub(super) undated_tasks: usize,
    pub(super) in_window: Vec<(usize, CohortTask)>,
    pub(super) lead_rows: Vec<(usize, CohortTask)>,
}

impl CohortWindowData {
    pub(super) fn load(
        runtime: &OrbitRuntime,
        range: &DiagnosticsRange,
        filters: &CohortFilters,
        day_lead: bool,
    ) -> Result<Self, OrbitError> {
        let read_since = if day_lead {
            TrendWindow::read_since_with_day_lead(range)
        } else {
            TrendWindow::read_since(range)
        };
        let dates = runtime.task_terminal_dates(read_since)?;
        let transitions: Vec<_> = dates
            .dated
            .iter()
            .filter(|row| filters.in_outcome(row.task.status))
            .collect();
        let earliest = transitions.first().map(|row| row.terminal_at);
        let window = if day_lead {
            TrendWindow::with_day_lead(range, earliest)
        } else {
            TrendWindow::new(range, earliest)
        };
        let undated_tasks = dates
            .undated
            .iter()
            .filter(|task| {
                filters.in_outcome(task.status)
                    && filters.wants_complexity(task.complexity.as_str())
                    && window.may_hold_undated(task.at)
            })
            .count();
        let select_tasks = |bucket_of: &dyn Fn(DateTime<Utc>) -> Option<usize>| {
            transitions
                .iter()
                .filter_map(|row| {
                    bucket_of(row.terminal_at)
                        .filter(|_| filters.wants_complexity(row.task.complexity.as_str()))
                        .map(|index| {
                            (
                                index,
                                CohortTask {
                                    task_id: row.task.task_id.clone(),
                                    complexity: row.task.complexity.clone(),
                                    tags: row.task.tags.clone(),
                                    created_at: row.task.created_at,
                                },
                            )
                        })
                })
                .collect::<Vec<_>>()
        };
        let in_window = select_tasks(&|at| window.bucket_of(at));
        // A window with no tasks draws nothing, so its lead is never read.
        let lead_rows = if !day_lead || in_window.is_empty() {
            Vec::new()
        } else {
            select_tasks(&|at| window.lead_bucket_of(at))
        };
        Ok(Self {
            window,
            undated_tasks,
            in_window,
            lead_rows,
        })
    }

    /// No linked invocation predates its task, so the oldest task in the
    /// window bounds the invocation read, with a day's slack for clock skew.
    /// An owner that omits `created_at` forces a full read.
    pub(super) fn invocation_read_since(&self) -> Option<DateTime<Utc>> {
        self.in_window
            .iter()
            .chain(&self.lead_rows)
            .map(|(_, row)| row.created_at)
            .collect::<Option<Vec<_>>>()
            .and_then(|created| created.into_iter().min())
            .map(|oldest| oldest - READ_SLACK)
    }

    pub(super) fn bucket_starts_json(&self) -> Vec<String> {
        (0..self.window.bucket_count())
            .map(|index| self.window.bucket_start(index).to_rfc3339())
            .collect()
    }
}

/// Sort `(label, count)` pairs descending by count then ascending by label.
pub(super) fn sorted_counts(counts: BTreeMap<&str, u64>) -> Vec<(&str, u64)> {
    let mut entries: Vec<(&str, u64)> = counts.into_iter().collect();
    entries.sort_by(|(left, left_n), (right, right_n)| {
        right_n.cmp(left_n).then_with(|| left.cmp(right))
    });
    entries
}

/// Project tag counts into the capped `available_tags` response array.
pub(super) fn available_tags_json(tag_counts: BTreeMap<&str, u64>) -> Vec<Value> {
    let mut tags = sorted_counts(tag_counts);
    tags.truncate(AVAILABLE_TAGS);
    tags.into_iter()
        .map(|(tag, tasks)| json!({ "tag": tag, "tasks": tasks }))
        .collect()
}
