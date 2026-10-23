//! Admission passes, forecasts, gap fit and resource pressure.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{DrainCapacity, DrainWaitingTask};

/// What a drain's most recent admission pass left waiting.
///
/// The classifier's own output lives only inside the running loop, so this is
/// the one durable record of the backlog a finished drain never started. It is
/// overwritten every pass: the last one is the drain's final view.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DrainAdmissionPass {
    /// Stable classifier of the latest failed pass, when available. A skew
    /// failure is terminal even when the drain window remains open. A local
    /// drain records `lock_busy` for a pass it deferred because a task-store
    /// lock stayed held past its acquisition deadline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_pass_error_code: Option<String>,
    pub recorded_at: DateTime<Utc>,
    /// Capacity at this pass, before it dispatches new work. Absent in older
    /// records and in pull passes, which use owner-side admission.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capacity: Option<DrainCapacity>,
    /// Admissible tasks the pass did not admit because no slot was free or
    /// they were deferred. Local passes include deferred tasks in this count;
    /// pull passes use the owner's queue depth, which excludes deferred tasks.
    pub queued: u64,
    /// Tasks a lock conflict or owner hold deferred. This is a subset of
    /// `queued` for local passes and separate from `queued` for pull passes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deferred: Vec<DrainWaitingTask>,
    /// The full count behind `deferred`. A pull drain bounds that list; a
    /// record that predates this field reads 0, so readers take the larger of
    /// the two.
    #[serde(default)]
    pub deferred_total: u64,
    /// Backlog tasks the drain could not admit at all (bounded list).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub excluded: Vec<DrainWaitingTask>,
    /// The full count behind `excluded`.
    #[serde(default)]
    pub excluded_total: u64,
    /// When the owner receipt behind `queued`, `deferred` and `excluded` was
    /// recorded. Set by a pull drain, whose pass keeps the last answer it
    /// has while it sends no request (throttled, held, owner unreachable), so
    /// it can be older than `recorded_at`. A local drain classifies afresh
    /// every pass and leaves it unset, except on a pass it deferred on a busy
    /// lock, which keeps the previous pass's view with the time it was read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting_recorded_at: Option<DateTime<Utc>>,
    /// Backlog tasks kept off this host, by reason code: the full count
    /// behind `deferred` and `excluded`, which are bounded. Pull drains only.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub waiting_by_reason: BTreeMap<String, u64>,
    /// Consecutive owner answers that left tasks waiting and claimed none, up
    /// to this pass. Pull drains only.
    #[serde(default)]
    pub consecutive_idle_passes: u32,
    /// Host resource pressure that held this pass's admissions [ORB-13901].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_throttle: Option<ResourceThrottle>,
    /// Why no task pilot prepares this workspace's tasks on the machine that
    /// owns it: its task-pilot routine is absent, owned by another machine,
    /// disabled or paused here, so tasks that need a pilot assessment wait
    /// without one ever coming. Names the workspace, the machine and the
    /// fix. Set afresh each local pass; pull passes leave it unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_pilot_warning: Option<String>,
    /// [ORB-15637] Why this pull pass requested no new claim although its
    /// window was open: the owner's probe refusal, a refused settlement
    /// (`settlement_refused`), the failure breaker (`circuit_open`), a host
    /// shutdown inside the hold horizon (`host_shutdown_scheduled`), or no
    /// runnable crew (`no_runnable_crew`, `host_suppressed`), or a bounded
    /// backoff from an owner whose store stayed locked
    /// (`owner_busy_backoff`). Set afresh each
    /// pass, never carried. Host pressure and degradation have their own
    /// fields. Pull drains only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admission_refusal: Option<String>,
    /// The latest failed pull pass, or a local pass deferred on a busy lock
    /// (naming the holder); cleared by success before degradation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_pass_error: Option<String>,
    /// Consecutive failed pull passes, independent of failed claimed leaves,
    /// or consecutive deferred local passes; a local drain fails when this
    /// reaches `TRANSIENT_PASS_FAILURE_LIMIT` (orbit-core's
    /// `application::workspace_auto::classify`).
    #[serde(default)]
    pub consecutive_pass_failures: u32,
    /// Sticky warning state: new admissions stop, but settlements keep flowing.
    /// Start a new drain after fixing the cause.
    #[serde(default)]
    pub degraded: bool,
    /// The gap-fit decisions of a local pass under
    /// its effective auto-concurrency mode `shadow` or `enforce`. Absent with the
    /// mode `off`, and in pull passes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admission_forecast: Option<DrainAdmissionForecast>,
}

/// One local pass's gap-fit admission against the host load forecast.
///
/// In `enforce` it is what the pass admitted; in `shadow` it is what enforce
/// would have admitted from the same candidates, while the pass admitted as
/// `off` does.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DrainAdmissionForecast {
    /// `shadow` or `enforce`.
    pub mode: String,
    /// When the forecast was built; absent when the pass had no free slot or
    /// candidate to fit, so it built none and carries only `head`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forecast_at: Option<DateTime<Utc>>,
    /// The forecast's highest CPU over its horizon, before this pass admitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_peak: Option<ForecastPeak>,
    /// The forecast's highest memory over its horizon, before this pass
    /// admitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_peak: Option<ForecastPeak>,
    /// The forecast's first hour, before this pass admitted, as a share of
    /// the host; absent when the host could not be measured, and in records
    /// written before it was kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub series: Option<ForecastBuckets>,
    /// Candidates that fit, in the order the pass admitted them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub admitted: Vec<GapFitAdmission>,
    /// Candidates that did not fit, or that the starvation guard held.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<GapFitSkip>,
    /// The first candidate in priority order while it fails gap fit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<GapFitHead>,
}

/// A forecast resource's highest value over the horizon.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ForecastPeak {
    /// Minutes from the forecast.
    pub minute: u32,
    /// Cores for CPU, bytes for memory.
    pub value: f64,
    /// The value as a share of the host's cores or memory; absent when the
    /// host could not be measured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub percent: Option<f64>,
}

/// A forecast's CPU and memory over minutes `0 ..= 60`, one point per
/// `bucket_minutes`, each as a share of the host's cores or memory.
///
/// Point 0 is minute 0 and point `k` the highest minute in
/// `(bucket_minutes · (k - 1), bucket_minutes · k]`, so a spike shorter than a
/// bucket still shows and the highest point is the peak whenever the peak
/// falls within the hour. A forecast whose horizon ends sooner has fewer
/// points.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ForecastBuckets {
    /// Minutes between points: 5.
    pub bucket_minutes: u32,
    pub cpu_percent: Vec<f64>,
    pub memory_percent: Vec<f64>,
}

/// A candidate gap fit admitted.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GapFitAdmission {
    pub task_id: String,
    /// The fallback step that found its profile: `exact`,
    /// `without_complexity`, `base_class`, `heaviest_of_class`,
    /// `heaviest_on_host`, or `none` on a host without profiles.
    pub profile_source: String,
    /// Set when it did not fit but the forecast held no running delivery or
    /// reservation, so it was admitted anyway: where it first exceeded its
    /// threshold, and whether its profile did so on its own or only with the
    /// host baseline, e.g. `cpu 125% at +3m on its own; admitted on an idle
    /// host`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_host: Option<String>,
}

/// A candidate gap fit kept out of the pass.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GapFitSkip {
    pub task_id: String,
    pub profile_source: String,
    /// `cpu` or `memory`; absent when the starvation guard held it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<String>,
    /// The first minute the forecast with this candidate exceeds its
    /// threshold.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minute: Option<u32>,
    /// The forecast at that minute, with the candidate, as a share of the
    /// host's cores or memory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub percent: Option<f64>,
    /// `cpu 94% at +6m`, or the head the starvation guard protects.
    pub detail: String,
}

/// The queue head while it fails gap fit, for the starvation guard.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GapFitHead {
    pub task_id: String,
    /// The first pass, of an unbroken run of passes, in which it failed.
    pub failing_since: DateTime<Utc>,
    /// It has failed for longer than `head_max_wait_minutes`, so nothing else
    /// is admitted until it fits.
    pub protected: bool,
    /// Where it failed in the last pass that judged it; carried through
    /// passes that judge no head.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub miss: Option<GapFitHeadMiss>,
    /// The protected passes over which its miss has not improved; absent
    /// while it is not protected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stall: Option<GapFitHeadStall>,
}

/// Where the queue head failed gap fit, and what it was priced and judged
/// against.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GapFitHeadMiss {
    /// The fallback step that found its profile, as in [`GapFitSkip`].
    pub profile_source: String,
    /// The key of the profile that step found: its work class and, when it
    /// has one, its complexity, e.g. `change/medium`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_key: Option<String>,
    /// `cpu` or `memory`.
    pub resource: String,
    /// The first minute the forecast with the head exceeds its threshold.
    pub minute: u32,
    /// The forecast at that minute, with the head, as a share of the host.
    pub percent: f64,
    /// The host baseline `B` of `resource`, the load outside deliveries, as
    /// a share of the host; absent when it could not be measured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline_percent: Option<f64>,
}

/// A protected head's run of passes without a better miss.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GapFitHeadStall {
    /// The miss the run is measured against: the head's miss in its first
    /// protected pass, or in the last pass that improved on the one before.
    pub reference: GapFitHeadMiss,
    /// When the pass that set `reference` ran.
    pub since: DateTime<Utc>,
    /// Consecutive protected passes, counting the one that set `reference`,
    /// whose miss did not improve on it.
    pub passes: u32,
    /// `passes` has reached `head_stuck_passes`: the head is stuck, and the
    /// drain says so until its miss improves or it stops being the head.
    #[serde(default)]
    pub stuck: bool,
    /// The friction an `enforce` drain filed once the head became stuck.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub friction_id: Option<String>,
}

/// One host resource whose sustained pressure holds new admissions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResourcePressure {
    /// `cpu`, `memory`, `io full` or `disk <path>`.
    pub resource: String,
    pub percent: f64,
    pub high_percent: u8,
    pub resume_percent: u8,
    /// When the reading crossed its high mark.
    pub since: DateTime<Utc>,
}

/// Sustained host resource pressure holding new admissions [ORB-13901].
///
/// A throttle starts no new task; running work is never cancelled, paused or
/// killed. Admission resumes once every listed resource is back below its
/// resume mark.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResourceThrottle {
    pub resources: Vec<ResourcePressure>,
}

impl ResourceThrottle {
    /// `memory 93% (throttled at ≥ 90% since 2026-10-04 08:41Z; resumes below 80%); cpu 97% (throttled at ≥ 90% since 2026-10-04 08:40Z; resumes below 75%)`.
    #[must_use]
    pub fn describe(&self) -> String {
        self.resources
            .iter()
            .map(|pressure| {
                format!(
                    "{} {:.0}% (throttled at \u{2265} {}% since {}; resumes below {}%)",
                    pressure.resource,
                    pressure.percent,
                    pressure.high_percent,
                    pressure.since.format("%Y-%m-%d %H:%MZ"),
                    pressure.resume_percent,
                )
            })
            .collect::<Vec<_>>()
            .join("; ")
    }

    /// The kind of each pressured resource: `cpu`, `memory`, `io` or `disk`.
    #[must_use]
    pub fn kinds(&self) -> BTreeSet<String> {
        self.resources
            .iter()
            .filter_map(|pressure| pressure.resource.split_whitespace().next())
            .map(str::to_string)
            .collect()
    }

    /// The operator-facing hold sentence every surface prints.
    #[must_use]
    pub fn hold_reason(&self) -> String {
        format!(
            "Admissions throttled: {}. Running work is not touched.",
            self.describe()
        )
    }
}
