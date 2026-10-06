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
//! Because five of the seven outcomes sit outside the denominator,
//! `succeeded + failed` does **not** equal the run count in general. Callers
//! render [`OutcomeCounts::settled`] as the `n` and report the excluded
//! buckets separately; [`OutcomeCounts::total`] is kept so the gap is visible
//! rather than implied away.
//!
//! # Recovery attribution
//!
//! Which activity performs recovery is a property of a workspace's job
//! definitions, never of Orbit, so the recovery activity set is discovered by
//! walking the job catalog ([`JobV2::activity_roles`]) at query time. An
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
    pub failed: u64,
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

    /// Runs that reached an outcome the failure rate is defined over.
    ///
    /// This is the rate's `n`. It is smaller than [`OutcomeCounts::total`]
    /// whenever runs were cancelled, skipped, held, still in flight, or in a state
    /// this build cannot parse.
    pub fn settled(&self) -> u64 {
        self.succeeded + self.failed
    }

    /// Runs excluded from the denominator, for the "not counted" disclosure.
    pub fn excluded(&self) -> u64 {
        self.cancelled + self.skipped + self.held + self.in_flight + self.unknown
    }

    /// Failure rate over settled runs, or `None` when nothing settled.
    pub fn failure_rate(&self) -> Rate {
        Rate::new(
            self.failed,
            self.settled(),
            "settled runs (success + failed)",
        )
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
        let job_runs = aggregate_job_runs(&facts.facts, window, facts.truncated);

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
}

/// Buckets and totals the per-run facts. Pure, so it is exercised directly by
/// unit tests without a store.
pub fn aggregate_job_runs(
    facts: &[JobRunOutcomeFact],
    window: &ReliabilityWindow,
    truncated: bool,
) -> JobRunReliability {
    let mut overall = OutcomeCounts::default();
    let mut by_job: BTreeMap<String, OutcomeCounts> = BTreeMap::new();
    let mut by_bucket: BTreeMap<DateTime<Utc>, OutcomeCounts> = BTreeMap::new();
    let mut observed_states: BTreeMap<String, u64> = BTreeMap::new();

    for fact in facts {
        let outcome = RunOutcome::classify(&fact.state);
        overall.record(outcome);
        by_job
            .entry(fact.job_id.clone())
            .or_default()
            .record(outcome);
        by_bucket
            .entry(window.bucket.floor(fact.created_at))
            .or_default()
            .record(outcome);
        *observed_states.entry(fact.state.clone()).or_default() += 1;
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
