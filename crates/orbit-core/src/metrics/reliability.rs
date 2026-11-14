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
//! | [`RunOutcome::UpgradePaused`] | `interrupted` under `upgrade_quiesce` | no |
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
//! An `interrupted` run whose recorded error code is `upgrade_quiesce` is a
//! designed pause, not a lost owner: an Orbit upgrade stopped it at a step
//! boundary and the clock resumes it, so it counts as
//! [`OutcomeCounts::upgrade_paused`]. The state alone cannot say so, so
//! [`RunOutcome::classify`] still reads such a run as failed and the fold below
//! separates it.
//!
//! A run the host manager stopped — a terminating signal whose recorded
//! sender command is `systemd`, with no recorded memory or task-limit breach —
//! is `held` under `operator_stopped`. That state is [`RunOutcome::Held`],
//! outside the failure rate. An unknown or unreadable sender, a signal from
//! another command, and a crash exit stay `interrupted` and count as failures.
//!
//! Because six of the eight outcomes sit outside the denominator, and a
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
//! workspace counts, the fleet totals. A delivery paused by an upgrade folds
//! the same way, with its representative in [`OutcomeCounts::upgrade_paused`].
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

pub use crate::application::job::catalog::JobDefinitionReads;
use crate::runtime::OrbitRuntime;
use crate::runtime::upgrade_handover::UPGRADE_QUIESCE_ERROR_CODE;

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
    /// Never returned by [`Self::classify`], which reads only the state: an
    /// upgrade's designed pause is told apart by its recorded error code.
    UpgradePaused,
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
    /// Deliveries an Orbit upgrade paused for the clock to resume. Outside the
    /// failure rate, and counted in [`Self::excluded`].
    pub upgrade_paused: u64,
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
            RunOutcome::UpgradePaused => self.upgrade_paused += 1,
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
    /// cancelled, skipped, held, paused by an upgrade, still in flight,
    /// unrecognized, or rolled up as wrapper echoes.
    pub fn settled(&self) -> u64 {
        self.succeeded + self.failed
    }

    /// Runs excluded from the denominator, for the "not counted" disclosure.
    pub fn excluded(&self) -> u64 {
        self.upgrade_paused
            + self.cancelled
            + self.skipped
            + self.held
            + self.in_flight
            + self.unknown
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
        self.pipeline_reliability_sharing_reads(window, &JobDefinitionReads::default())
    }

    /// [`Self::pipeline_reliability`] for one workspace of a fleet read.
    ///
    /// The request shares `definition_reads` for its initial definition
    /// snapshot, so shared definition files are listed and read once for that
    /// snapshot. A matching warm role memo reuses parsed roles. On a cold or
    /// invalidated memo miss, this workspace's catalog is loaded and parsed
    /// independently. When the initial snapshot is readable and parsing
    /// returns no diagnostics, a fresh definition snapshot is read after
    /// parsing to verify the inputs did not change before memoizing. Catalogs
    /// with diagnostics are not memoized, so their per-workspace parsing
    /// repeats on later requests.
    pub fn pipeline_reliability_sharing_reads(
        &self,
        window: &ReliabilityWindow,
        definition_reads: &JobDefinitionReads,
    ) -> Result<RuntimeReliability, OrbitError> {
        if window.since >= window.until {
            return Err(OrbitError::InvalidInput(
                "reliability window `since` must be earlier than `until`".to_string(),
            ));
        }
        // The partition `job_runs` and `invocations` are recorded under.
        let workspace_id = self.stores().jobs().workspace_id().to_string();
        let store = self.sqlite_store()?;
        let mut phase = PhaseClock::start();

        let facts = store.list_job_run_outcome_facts(
            &workspace_id,
            window.since,
            window.until,
            MAX_JOB_RUN_FACTS,
        )?;
        let facts_us = phase.lap();
        let echoes = self.failure_echoes(&facts.facts)?;
        let echoes_us = phase.lap();
        let job_runs = aggregate_job_runs(&facts.facts, window, facts.truncated, &echoes);

        let counts =
            store.count_invocations_by_activity(&workspace_id, window.since, window.until)?;
        let invocations_us = phase.lap();
        let roles = self.job_activity_roles(definition_reads)?;
        let roles_us = phase.lap();
        // Distinct counts do not sum across activities.
        let recovery_ids = roles.recovery_only().into_iter().collect::<Vec<_>>();
        let coverage = store.count_invocation_job_runs(
            &workspace_id,
            window.since,
            window.until,
            &recovery_ids,
        )?;
        let coverage_us = phase.lap();
        let recovery = aggregate_recovery(&counts, &roles, coverage);
        tracing::debug!(
            workspace = %workspace_id,
            facts = facts.facts.len(),
            echoed = echoes.leaves.len(),
            activities = counts.len(),
            facts_us,
            echoes_us,
            invocations_us,
            roles_us,
            coverage_us,
            "pipeline reliability phases"
        );

        Ok(RuntimeReliability {
            window: window.clone(),
            job_runs,
            recovery,
        })
    }

    /// The leaf each failed run that recorded a root cause echoes.
    ///
    /// One batch read of the failed ids only. Runs that failed on their own,
    /// and runs whose stored state is missing or unreadable, are absent: they
    /// stay their own leaf.
    fn failure_echoes(&self, facts: &[JobRunOutcomeFact]) -> Result<FailureEchoes, OrbitError> {
        let failed_ids = facts
            .iter()
            .filter(|fact| RunOutcome::classify(&fact.state) == RunOutcome::Failed)
            .map(|fact| fact.run_id.clone())
            .collect::<Vec<_>>();
        if failed_ids.is_empty() {
            return Ok(FailureEchoes::default());
        }
        let mut echoes = FailureEchoes::default();
        for (run_id, state) in self.read_run_states(&failed_ids)? {
            let Some(cause) = state.and_then(|state| state.root_cause) else {
                continue;
            };
            if cause.leaf_run_id.is_empty() || cause.leaf_run_id == run_id {
                continue;
            }
            if cause.code.as_deref() == Some(UPGRADE_QUIESCE_ERROR_CODE) {
                echoes.upgrade_paused.insert(run_id.clone());
            }
            echoes.leaves.insert(run_id, cause.leaf_run_id);
        }
        Ok(echoes)
    }
}

/// Microsecond laps for the per-workspace phase breakdown logged at debug.
struct PhaseClock(std::time::Instant);

impl PhaseClock {
    fn start() -> Self {
        Self(std::time::Instant::now())
    }

    fn lap(&mut self) -> u128 {
        let now = std::time::Instant::now();
        let elapsed = now.duration_since(self.0).as_micros();
        self.0 = now;
        elapsed
    }
}

/// What failed runs recorded about the leaf they only echo.
#[derive(Debug, Clone, Default)]
pub struct FailureEchoes {
    /// `run_id -> leaf_run_id`. A failed run absent from it is its own leaf.
    pub leaves: BTreeMap<String, String>,
    /// The echoes whose recorded root cause is an upgrade's pause. Decides a
    /// delivery whose leaf is not in the window, as on another host; a leaf in
    /// the window decides for itself.
    pub upgrade_paused: BTreeSet<String>,
}

/// How one failed run counts once its delivery is folded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailureFold {
    Cascaded,
    Failed,
    UpgradePaused,
}

/// Buckets and totals the per-run facts.
///
/// `echoes` names the leaf each echoing failed run stands for. Facts are
/// otherwise unchanged.
pub fn aggregate_job_runs(
    facts: &[JobRunOutcomeFact],
    window: &ReliabilityWindow,
    truncated: bool,
    echoes: &FailureEchoes,
) -> JobRunReliability {
    let mut overall = OutcomeCounts::default();
    let mut by_job: BTreeMap<String, OutcomeCounts> = BTreeMap::new();
    let mut by_bucket: BTreeMap<DateTime<Utc>, OutcomeCounts> = BTreeMap::new();
    let mut observed_states: BTreeMap<String, u64> = BTreeMap::new();
    let folds = failure_folds(facts, echoes);

    for (index, fact) in facts.iter().enumerate() {
        *observed_states.entry(fact.state.clone()).or_default() += 1;
        let outcome = RunOutcome::classify(&fact.state);
        let counted = match (outcome, folds[index]) {
            (RunOutcome::Failed, FailureFold::Cascaded) => None,
            (RunOutcome::Failed, FailureFold::UpgradePaused) => Some(RunOutcome::UpgradePaused),
            _ => Some(outcome),
        };
        match counted {
            Some(counted) => record_outcome(
                &mut overall,
                &mut by_job,
                &mut by_bucket,
                window,
                fact,
                counted,
            ),
            None => record_cascaded(&mut overall, &mut by_job, &mut by_bucket, window, fact),
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

/// How each failed run counts: the one that represents its delivery as failed
/// or paused by an upgrade, every other as cascaded.
///
/// The leaf itself represents the delivery when it is in `facts`. Otherwise
/// the latest echo does, so a leaf outside this store still counts once and
/// the extra echoes stay cascaded. Ties break on run id. A leaf in `facts`
/// decides from its own recorded code, so an echo cannot hide a real
/// interruption; without one, any echo naming an upgrade's pause decides.
fn failure_folds(facts: &[JobRunOutcomeFact], echoes: &FailureEchoes) -> Vec<FailureFold> {
    let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (index, fact) in facts.iter().enumerate() {
        if RunOutcome::classify(&fact.state) != RunOutcome::Failed {
            continue;
        }
        let key = failure_key(&fact.run_id, &echoes.leaves);
        groups.entry(key).or_default().push(index);
    }
    let mut folds = vec![FailureFold::Cascaded; facts.len()];
    for (key, indexes) in groups {
        let leaf = indexes
            .iter()
            .copied()
            .find(|index| facts[*index].run_id == key);
        let upgrade_paused = leaf.map_or_else(
            || {
                indexes
                    .iter()
                    .any(|index| echoes.upgrade_paused.contains(&facts[*index].run_id))
            },
            |leaf| {
                facts[leaf].interrupted_error_code.as_deref() == Some(UPGRADE_QUIESCE_ERROR_CODE)
            },
        );
        let chosen = leaf.or_else(|| {
            indexes.iter().copied().max_by(|left, right| {
                facts[*left]
                    .created_at
                    .cmp(&facts[*right].created_at)
                    .then_with(|| facts[*right].run_id.cmp(&facts[*left].run_id))
            })
        });
        if let Some(chosen) = chosen {
            folds[chosen] = if upgrade_paused {
                FailureFold::UpgradePaused
            } else {
                FailureFold::Failed
            };
        }
    }
    folds
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
