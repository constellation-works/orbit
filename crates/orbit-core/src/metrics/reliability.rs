//! Pipeline-reliability aggregation over durable run state [ORB-10588].
//!
//! Answers two operator questions that were previously only reachable by
//! opening SQLite by hand: how often job runs fail, and how often the recovery
//! path fires.
//!
//! # Inputs
//!
//! Everything here derives from persisted `job_runs` and `invocations` rows.
//! No agent-loop output, no token field, and no cost field is read — see
//! [`orbit_store::contracts::JobRunOutcomeFact`] and
//! [`orbit_store::contracts::ActivityInvocationCount`] for the projected column sets.
//!
//! # Outcome classification
//!
//! `job_runs.state` is not two-valued, so a "failure rate" has to say which
//! states it counts. [`RunOutcome`] partitions the [`JobRunState`] variants:
//!
//! | outcome | states | in failure-rate denominator |
//! |---|---|---|
//! | [`RunOutcome::Succeeded`] | `success` | yes |
//! | [`RunOutcome::Failed`] | `failed`, `timeout`, `interrupted` | yes |
//! | [`RunOutcome::Cancelled`] | `cancelled` | no |
//! | [`RunOutcome::Skipped`] | `skipped` | no |
//! | [`RunOutcome::Held`] | `held` | no |
//! | [`RunOutcome::InFlight`] | `pending`, `running`, `retrying` | no |
//! | [`RunOutcome::Unknown`] | any state this build cannot parse | no |
//!
//! `timeout` and `interrupted` count as failures: both are runs the pipeline
//! intended to finish and did not. `interrupted` specifically means the owning
//! process died without finalizing, which is a reliability event even though
//! the run's checkpoints can seed a resumed follow-up. `cancelled` is a
//! deliberate operator action, so counting it as a failure would make manual
//! intervention look like breakage. `skipped` never ran.
//!
//! Because five of the seven outcomes sit outside the denominator, and a
//! wrapper that failed only because its child failed is rolled out of
//! `failed` into [`OutcomeCounts::cascaded`], `succeeded + failed` does
//! **not** equal the run count in general. Callers render
//! [`OutcomeCounts::settled`] as the `n` and report the excluded and cascaded
//! buckets separately; [`OutcomeCounts::total`] is kept so the gap is visible
//! rather than implied away. `total = settled + excluded + cascaded`.
//!
//! # Wrapper echoes
//!
//! A failed, timed-out, or interrupted run whose pipeline state records a
//! `root_cause.leaf_run_id` failed in `pipeline_success_guard` because that
//! leaf failed. Those runs are one delivery. The failure count is the number
//! of distinct leaves; a run with no recorded root cause is its own leaf.
//! The representative of a leaf is that run when it is in the window, and
//! otherwise the latest echo, so a leaf that settled on another host still
//! counts once. Every other echo is [`OutcomeCounts::cascaded`] and sits
//! outside both the numerator and the denominator. The same fold feeds the
//! by-job rows, the over-time series, and, by summing those already-folded
//! workspace counts, the fleet totals.
//!
//! # Recovery attribution
//!
//! Which activity performs recovery is a property of a workspace's job
//! definitions, never of Orbit, so the recovery activity set is discovered by
//! walking the job catalog ([`JobV2::activity_roles`](orbit_types::workflow::JobV2::activity_roles)) at query time. An
//! activity that a catalog uses in both roles is reported as ambiguous and
//! excluded from the numerator, since its invocations cannot be attributed
//! structurally.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;

use orbit_common::OrbitError;
use orbit_store::contracts::{ActivityInvocationCount, InvocationRunCoverage, JobRunOutcomeFact};
use orbit_types::workflow::{JobActivityRoles, JobRunState};

use crate::runtime::OrbitRuntime;

/// Row cap for the per-run fact read backing the failure rate. Reaching it
/// sets [`JobRunReliability::truncated`]; the cap is never applied silently.
const MAX_JOB_RUN_FACTS: usize = 200_000;

/// Denominator below which a rate is flagged [`Rate::low_sample`]. Callers
/// mark or withhold such cells instead of rendering a confident percentage.
pub const MIN_CONFIDENT_SAMPLE: u64 = 20;

/// What the failure-rate denominator counts. The UI renders this verbatim, so
/// the wrapper roll-up is part of the number rather than only a code comment.
const FAILURE_DENOMINATOR_LABEL: &str = "settled deliveries (success + one count per failed \
     delivery; wrapper echoes rolled into the leaf)";

/// Bucket width for the over-time breakdown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BucketGranularity {
    Hour,
    Day,
}

impl BucketGranularity {
    fn width(self) -> Duration {
        match self {
            Self::Hour => Duration::hours(1),
            Self::Day => Duration::days(1),
        }
    }

    /// Floors `at` to the start of its bucket.
    fn floor(self, at: DateTime<Utc>) -> DateTime<Utc> {
        let seconds = self.width().num_seconds().max(1);
        let epoch = at.timestamp();
        let floored = epoch.div_euclid(seconds) * seconds;
        DateTime::from_timestamp(floored, 0).unwrap_or(at)
    }
}

/// The explicit, half-open window (`since <= t < until`) every rate is scoped
/// to. Carried through to the payload so no rate can be rendered without it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReliabilityWindow {
    /// Caller-supplied label for the window (e.g. `"7d"`), echoed to the UI.
    pub label: String,
    pub since: DateTime<Utc>,
    pub until: DateTime<Utc>,
    pub bucket: BucketGranularity,
}

impl ReliabilityWindow {
    /// Builds a window ending at `until` and spanning `duration`, choosing a
    /// bucket width that keeps the series legible.
    pub fn ending_at(label: impl Into<String>, until: DateTime<Utc>, duration: Duration) -> Self {
        let bucket = if duration <= Duration::days(2) {
            BucketGranularity::Hour
        } else {
            BucketGranularity::Day
        };
        Self {
            label: label.into(),
            since: until - duration,
            until,
            bucket,
        }
    }
}

/// How a persisted `job_runs.state` is counted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOutcome {
    Succeeded,
    Failed,
    Cancelled,
    Skipped,
    Held,
    InFlight,
    Unknown,
}

impl RunOutcome {
    /// Classifies a raw `job_runs.state` string.
    ///
    /// Parsing is deliberately tolerant: a state written by a newer binary
    /// lands in [`RunOutcome::Unknown`] and stays visible in the counts rather
    /// than being silently folded into success or failure.
    pub fn classify(raw: &str) -> Self {
        let Ok(state) = raw.parse::<JobRunState>() else {
            return Self::Unknown;
        };
        match state {
            JobRunState::Success => Self::Succeeded,
            JobRunState::Failed | JobRunState::Timeout | JobRunState::Interrupted => Self::Failed,
            JobRunState::Cancelled => Self::Cancelled,
            JobRunState::Skipped => Self::Skipped,
            JobRunState::Held => Self::Held,
            JobRunState::Pending | JobRunState::Running | JobRunState::Retrying => Self::InFlight,
        }
    }
}

/// Run counts for one scope (overall, one job, or one time bucket).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct OutcomeCounts {
    /// Every run in scope, including the ones outside the denominator.
    pub total: u64,
    pub succeeded: u64,
    /// Distinct failed deliveries. A wrapper that only echoes a child is not
    /// counted here; it is counted in [`Self::cascaded`].
    pub failed: u64,
    /// Wrapper runs that failed only because a child failed. Rolled into that
    /// child's [`Self::failed`] count and omitted from [`Self::settled`] and
    /// [`Self::excluded`], so `total = settled + excluded + cascaded`.
    pub cascaded: u64,
    pub cancelled: u64,
    pub skipped: u64,
    pub held: u64,
    pub in_flight: u64,
    pub unknown: u64,
}

impl OutcomeCounts {
    fn record(&mut self, outcome: RunOutcome) {
        self.total += 1;
        match outcome {
            RunOutcome::Succeeded => self.succeeded += 1,
            RunOutcome::Failed => self.failed += 1,
            RunOutcome::Cancelled => self.cancelled += 1,
            RunOutcome::Skipped => self.skipped += 1,
            RunOutcome::Held => self.held += 1,
            RunOutcome::InFlight => self.in_flight += 1,
            RunOutcome::Unknown => self.unknown += 1,
        }
    }

    fn record_cascaded(&mut self) {
        self.total += 1;
        self.cascaded += 1;
    }

    /// Runs the failure rate is defined over.
    ///
    /// This is the rate's `n`: successes plus one count per failed delivery.
    /// It is smaller than [`OutcomeCounts::total`] whenever runs were
    /// cancelled, skipped, held, still in flight, unrecognized, or rolled up
    /// as wrapper echoes.
    pub fn settled(&self) -> u64 {
        self.succeeded + self.failed
    }

    /// Runs excluded from the denominator, for the "not counted" disclosure.
    pub fn excluded(&self) -> u64 {
        self.cancelled + self.skipped + self.held + self.in_flight + self.unknown
    }

    /// Failure rate over settled runs, or `None` when nothing settled.
    pub fn failure_rate(&self) -> Rate {
        Rate::new(self.failed, self.settled(), FAILURE_DENOMINATOR_LABEL)
    }
}

/// A rate that carries its own denominator, so no surface can render the
/// number without the `n` it was computed from.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Rate {
    pub numerator: u64,
    pub denominator: u64,
    /// Human-readable description of what the denominator counts. Rendered
    /// verbatim by the UI so the meaning never lives only in code.
    pub denominator_label: String,
    /// `numerator / denominator`, or `None` when the denominator is zero.
    pub value: Option<f64>,
    /// True when the denominator is below [`MIN_CONFIDENT_SAMPLE`]. Consumers
    /// mark or withhold the percentage instead of presenting it as confident.
    pub low_sample: bool,
}

impl Rate {
    pub fn new(numerator: u64, denominator: u64, denominator_label: impl Into<String>) -> Self {
        Self {
            numerator,
            denominator,
            denominator_label: denominator_label.into(),
            value: (denominator > 0).then(|| numerator as f64 / denominator as f64),
            low_sample: denominator < MIN_CONFIDENT_SAMPLE,
        }
    }
}

/// Counts for one job id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct JobOutcomeRow {
    pub job_id: String,
    #[serde(flatten)]
    pub counts: OutcomeCounts,
}

/// Counts for one time bucket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BucketOutcomeRow {
    pub bucket_start: DateTime<Utc>,
    pub bucket_end: DateTime<Utc>,
    #[serde(flatten)]
    pub counts: OutcomeCounts,
}

/// Job-run failure rate for one workspace over one window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct JobRunReliability {
    pub overall: OutcomeCounts,
    /// Descending by run count, so the noisiest job leads.
    pub by_job: Vec<JobOutcomeRow>,
    /// Ascending by bucket start, with empty buckets present so a gap in
    /// activity reads as a gap rather than as continuity.
    pub over_time: Vec<BucketOutcomeRow>,
    /// Every raw `job_runs.state` value observed in the window, with its
    /// count. This is the evidence behind the classification table: an
    /// operator can check what the store actually contains without querying
    /// it, and an unrecognized state shows up here by name.
    pub observed_states: BTreeMap<String, u64>,
    /// True when the window held more runs than the read cap.
    pub truncated: bool,
}

/// Recovery-path engagement for one workspace over one window.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RecoveryReliability {
    /// Activities the job catalog names as `recovery_activity`, discovered at
    /// query time. Empty when no job declares one — in which case the rate is
    /// not applicable rather than zero.
    pub recovery_activities: Vec<String>,
    /// Activities the catalog uses as both a step target and a recovery hook.
    /// Their invocations are excluded from the numerator because the store
    /// does not record which role a given invocation played.
    pub ambiguous_activities: Vec<String>,
    /// Recovery invocations per invocation of a declared step activity. This
    /// is the "how much extra work does recovery cost" reading.
    pub per_step_invocation: Rate,
    /// Share of job runs that recorded at least one invocation and had at
    /// least one recovery invocation. This is the "how often does a run need
    /// recovery at all" reading.
    pub per_job_run: Rate,
    /// Full per-activity invocation counts for the window, so the two rates
    /// above can be audited against the rows they came from.
    pub by_activity: Vec<ActivityCount>,
}

/// One activity's invocation counts, tagged with its discovered role.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ActivityCount {
    pub activity_id: String,
    pub invocation_count: u64,
    pub job_run_count: u64,
    pub role: ActivityRole,
}

/// The structural role a job catalog assigns an activity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityRole {
    /// Named only as a step target.
    Step,
    /// Named only as a `recovery_activity`.
    Recovery,
    /// Named in both roles; not attributable from the store alone.
    Ambiguous,
    /// Invoked but not named by any loaded job definition.
    Unknown,
}

/// Both reliability readings for one workspace, plus the window they share.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RuntimeReliability {
    pub window: ReliabilityWindow,
    pub job_runs: JobRunReliability,
    pub recovery: RecoveryReliability,
}

impl OrbitRuntime {
    /// Computes job-run failure rate and recovery engagement for this
    /// workspace over `window`, entirely from persisted store rows.
    pub fn pipeline_reliability(
        &self,
        window: &ReliabilityWindow,
    ) -> Result<RuntimeReliability, OrbitError> {
        if window.since >= window.until {
            return Err(OrbitError::InvalidInput(
                "reliability window `since` must be earlier than `until`".to_string(),
            ));
        }
        // The partition `job_runs` and `invocations` are recorded under.
        let workspace_id = self.stores().jobs().workspace_id().to_string();
        let store = self.sqlite_store()?;

        let facts = store.list_job_run_outcome_facts(
            &workspace_id,
            window.since,
            window.until,
            MAX_JOB_RUN_FACTS,
        )?;
        let echoed_leaves = self.failure_echoes(&facts.facts)?;
        let job_runs = aggregate_job_runs(&facts.facts, window, facts.truncated, &echoed_leaves);

        let counts =
            store.count_invocations_by_activity(&workspace_id, window.since, window.until)?;
        let roles = self.catalog_activity_roles()?;
        // The distinct-run denominator has to come from the store: distinct
        // counts do not sum across activities.
        let recovery_ids = roles.recovery_only().into_iter().collect::<Vec<_>>();
        let coverage = store.count_invocation_job_runs(
            &workspace_id,
            window.since,
            window.until,
            &recovery_ids,
        )?;
        let recovery = aggregate_recovery(&counts, &roles, coverage);

        Ok(RuntimeReliability {
            window: window.clone(),
            job_runs,
            recovery,
        })
    }

    /// Unions the activity roles declared by every job in the catalog.
    ///
    /// Disabled jobs are included: a job disabled midway through the window
    /// still produced the invocations being counted.
    fn catalog_activity_roles(&self) -> Result<JobActivityRoles, OrbitError> {
        use crate::application::job::JobCatalogFilter;

        let mut roles = JobActivityRoles::default();
        for (entry, _) in self.list_job_catalog_with_last_run(true, JobCatalogFilter::All)? {
            roles.merge(&entry.spec.activity_roles());
        }
        Ok(roles)
    }

    /// `run_id -> leaf_run_id` for failed runs that recorded a root cause.
    ///
    /// One batch read of the failed ids only. Runs that failed on their own,
    /// and runs whose stored state is missing or unreadable, are absent: they
    /// stay their own leaf.
    fn failure_echoes(
        &self,
        facts: &[JobRunOutcomeFact],
    ) -> Result<BTreeMap<String, String>, OrbitError> {
        let failed_ids = facts
            .iter()
            .filter(|fact| RunOutcome::classify(&fact.state) == RunOutcome::Failed)
            .map(|fact| fact.run_id.clone())
            .collect::<Vec<_>>();
        if failed_ids.is_empty() {
            return Ok(BTreeMap::new());
        }
        let mut echoed = BTreeMap::new();
        for (run_id, state) in self.read_run_states(&failed_ids)? {
            let Some(cause) = state.and_then(|state| state.root_cause) else {
                continue;
            };
            if cause.leaf_run_id.is_empty() || cause.leaf_run_id == run_id {
                continue;
            }
            echoed.insert(run_id, cause.leaf_run_id);
        }
        Ok(echoed)
    }
}

/// Buckets and totals the per-run facts.
///
/// `echoed_leaves` maps a failed run id to the leaf it echoes. A failed run
/// absent from the map is its own leaf. Facts are otherwise unchanged: the
/// store projection stays identifiers, state, and a timestamp.
pub fn aggregate_job_runs(
    facts: &[JobRunOutcomeFact],
    window: &ReliabilityWindow,
    truncated: bool,
    echoed_leaves: &BTreeMap<String, String>,
) -> JobRunReliability {
    let mut overall = OutcomeCounts::default();
    let mut by_job: BTreeMap<String, OutcomeCounts> = BTreeMap::new();
    let mut by_bucket: BTreeMap<DateTime<Utc>, OutcomeCounts> = BTreeMap::new();
    let mut observed_states: BTreeMap<String, u64> = BTreeMap::new();
    let representative = failure_representatives(facts, echoed_leaves);

    for (index, fact) in facts.iter().enumerate() {
        *observed_states.entry(fact.state.clone()).or_default() += 1;
        let outcome = RunOutcome::classify(&fact.state);
        if outcome == RunOutcome::Failed && !representative[index] {
            record_cascaded(&mut overall, &mut by_job, &mut by_bucket, window, fact);
        } else {
            record_outcome(
                &mut overall,
                &mut by_job,
                &mut by_bucket,
                window,
                fact,
                outcome,
            );
        }
    }

    let mut job_rows = by_job
        .into_iter()
        .map(|(job_id, counts)| JobOutcomeRow { job_id, counts })
        .collect::<Vec<_>>();
    job_rows.sort_by(|left, right| {
        right
            .counts
            .total
            .cmp(&left.counts.total)
            .then_with(|| left.job_id.cmp(&right.job_id))
    });

    JobRunReliability {
        overall,
        by_job: job_rows,
        over_time: fill_buckets(window, &by_bucket),
        observed_states,
        truncated,
    }
}

/// True for the one failed run that represents each delivery.
///
/// The leaf itself represents the delivery when it is in `facts`. Otherwise
/// the latest echo does, so a leaf outside this store still counts once and
/// the extra echoes stay cascaded. Ties break on run id.
fn failure_representatives(
    facts: &[JobRunOutcomeFact],
    echoed_leaves: &BTreeMap<String, String>,
) -> Vec<bool> {
    let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (index, fact) in facts.iter().enumerate() {
        if RunOutcome::classify(&fact.state) != RunOutcome::Failed {
            continue;
        }
        let key = failure_key(&fact.run_id, echoed_leaves);
        groups.entry(key).or_default().push(index);
    }
    let mut representative = vec![false; facts.len()];
    for (key, indexes) in groups {
        let chosen = indexes
            .iter()
            .copied()
            .find(|index| facts[*index].run_id == key)
            .or_else(|| {
                indexes.iter().copied().max_by(|left, right| {
                    facts[*left]
                        .created_at
                        .cmp(&facts[*right].created_at)
                        .then_with(|| facts[*right].run_id.cmp(&facts[*left].run_id))
                })
            });
        if let Some(chosen) = chosen {
            representative[chosen] = true;
        }
    }
    representative
}

/// Follows recorded echoes to the leaf, stopping on a cycle or a run that
/// failed on its own.
fn failure_key(run_id: &str, echoed_leaves: &BTreeMap<String, String>) -> String {
    let mut key = run_id.to_string();
    let mut seen = BTreeSet::new();
    while let Some(next) = echoed_leaves.get(&key) {
        if next.is_empty() || !seen.insert(key.clone()) {
            break;
        }
        key = next.clone();
    }
    key
}

fn record_outcome(
    overall: &mut OutcomeCounts,
    by_job: &mut BTreeMap<String, OutcomeCounts>,
    by_bucket: &mut BTreeMap<DateTime<Utc>, OutcomeCounts>,
    window: &ReliabilityWindow,
    fact: &JobRunOutcomeFact,
    outcome: RunOutcome,
) {
    overall.record(outcome);
    by_job
        .entry(fact.job_id.clone())
        .or_default()
        .record(outcome);
    by_bucket
        .entry(window.bucket.floor(fact.created_at))
        .or_default()
        .record(outcome);
}

fn record_cascaded(
    overall: &mut OutcomeCounts,
    by_job: &mut BTreeMap<String, OutcomeCounts>,
    by_bucket: &mut BTreeMap<DateTime<Utc>, OutcomeCounts>,
    window: &ReliabilityWindow,
    fact: &JobRunOutcomeFact,
) {
    overall.record_cascaded();
    by_job
        .entry(fact.job_id.clone())
        .or_default()
        .record_cascaded();
    by_bucket
        .entry(window.bucket.floor(fact.created_at))
        .or_default()
        .record_cascaded();
}

/// Materializes every bucket in the window, including empty ones, so the
/// series cannot imply activity across a quiet stretch.
fn fill_buckets(
    window: &ReliabilityWindow,
    observed: &BTreeMap<DateTime<Utc>, OutcomeCounts>,
) -> Vec<BucketOutcomeRow> {
    let width = window.bucket.width();
    let mut rows = Vec::new();
    let mut start = window.bucket.floor(window.since);
    // A pathological window cannot outrun this: the loop advances by a fixed
    // positive width and stops at `until`.
    while start < window.until {
        let end = start + width;
        rows.push(BucketOutcomeRow {
            bucket_start: start,
            bucket_end: end,
            counts: observed.get(&start).cloned().unwrap_or_default(),
        });
        start = end;
    }
    rows
}

/// Attributes invocation counts to discovered roles and derives both recovery
/// rates. Pure, for the same reason as [`aggregate_job_runs`].
pub fn aggregate_recovery(
    counts: &[ActivityInvocationCount],
    roles: &JobActivityRoles,
    coverage: InvocationRunCoverage,
) -> RecoveryReliability {
    let recovery_only = roles.recovery_only();
    let step_only = roles.step_only();
    let ambiguous = roles
        .recovery
        .intersection(&roles.step)
        .cloned()
        .collect::<BTreeSet<_>>();

    let mut recovery_invocations = 0_u64;
    let mut step_invocations = 0_u64;
    let mut by_activity = Vec::with_capacity(counts.len());

    for count in counts {
        let role = if ambiguous.contains(&count.activity_id) {
            ActivityRole::Ambiguous
        } else if recovery_only.contains(&count.activity_id) {
            ActivityRole::Recovery
        } else if step_only.contains(&count.activity_id) {
            ActivityRole::Step
        } else {
            ActivityRole::Unknown
        };
        match role {
            ActivityRole::Recovery => recovery_invocations += count.invocation_count,
            ActivityRole::Step => step_invocations += count.invocation_count,
            ActivityRole::Ambiguous | ActivityRole::Unknown => {}
        }
        by_activity.push(ActivityCount {
            activity_id: count.activity_id.clone(),
            invocation_count: count.invocation_count,
            job_run_count: count.job_run_count,
            role,
        });
    }

    RecoveryReliability {
        recovery_activities: recovery_only.into_iter().collect(),
        ambiguous_activities: ambiguous.into_iter().collect(),
        per_step_invocation: Rate::new(
            recovery_invocations,
            step_invocations,
            "step-activity invocations",
        ),
        per_job_run: Rate::new(
            coverage.matching_job_runs,
            coverage.total_job_runs,
            "job runs with any recorded invocation",
        ),
        by_activity,
    }
}
