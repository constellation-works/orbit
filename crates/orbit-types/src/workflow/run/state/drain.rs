//! Drain and crew policy state.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Drain controls, pull admission checkpoints, and delivery forge holds carried
/// by a pipeline run. Missing records retain the submitted/default behavior.
///
/// [`super::PipelineState::pull_drain`] flattens these fields into `state.json`;
/// the Rust grouping does not add an object or rename a persisted key.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PullDrainState {
    /// Live worker ceiling for a bounded auto drain, when an operator has
    /// adjusted it [ORB-11253]. Absent means the submitted input still
    /// governs. Like `child_dispatches` this survives terminalization: it is
    /// the evidence of what the run was actually admitting under.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drain_worker_limit: Option<DrainWorkerLimit>,
    /// Operator stop of *new* admissions on a bounded auto drain [ORB-11283].
    /// Absent means the drain is still admitting under its window and ceiling.
    /// Like `drain_worker_limit` this survives terminalization: it is how a
    /// finished coordinator is distinguished from cancellation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drain_admissions_stop: Option<DrainAdmissionsStop>,
    /// Operator extension of this drain's window [ORB-15383]. Absent means the
    /// deadline its `open_window` step stamped still governs. Survives
    /// terminalization like `drain_worker_limit`, as evidence of the window
    /// the run actually admitted under.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drain_window_extension: Option<DrainWindowExtension>,
    /// A graceful cancel this pull drain is carrying out: present while it
    /// waits for its launched leaves, and kept once it ends `cancelled`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drain_cancel: Option<DrainCancelRequest>,
    /// The provider preflight a pull drain took when its window opened.
    /// Survives terminalization: it is how `run show` reports which crews
    /// the drain could not run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_crew_preflight: Option<PullCrewPreflight>,
    /// Recovery per released auth incident; old incidents stay acknowledged on resume.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub pull_auth_recovery: BTreeMap<String, PullAuthRecovery>,
    /// The single admission pass of a pull drain submitted without a window,
    /// once it has started. Survives terminalization and resume, so the pass
    /// is never taken twice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_single_pass: Option<PullSinglePass>,
    /// The pass that closed this timed pull drain's window [ORB-15417]: once
    /// present, no extension reopens it. Survives terminalization and resume,
    /// so a retried pass never admits past it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_window_close: Option<PullWindowClose>,
    /// This pull drain's failure breaker while it is open or half-open
    /// [ORB-15639]; absent while closed. Survives terminalization, so `run
    /// show` reports the breaker a drain ended with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_breaker: Option<PullBreaker>,
    /// What this pull drain's host load forecast last had room for, under
    /// `machine.auto_concurrency` `shadow` or `enforce`; absent with `off` or
    /// before the host has resource profiles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_admission_fit: Option<PullAdmissionFit>,
    /// What this drain's last admission pass left waiting. Survives
    /// terminalization: it is how `run show` reports the backlog a finished
    /// drain never started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drain_last_pass: Option<DrainAdmissionPass>,
    /// The most recent local pass that built a forecast. Idle passes keep
    /// this observation, including its timestamp, decisions and peaks.
    /// Survives terminalization; absent before any forecast was built.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drain_last_forecast: Option<DrainAdmissionForecast>,
    /// What this `--approve-proposed` drain approved and held. Survives
    /// terminalization like `drain_last_pass`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drain_approvals: Option<DrainApprovalReport>,
    /// The push this run held at because the forge kept refusing it
    /// [ORB-14617]. Present only while the run's last outcome is that hold:
    /// finalization replaces it with the run's own result, and a resume
    /// seeded from a held run reads it to keep the lineage's `held_since`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forge_hold: Option<crate::workflow::ForgeUnavailableHold>,
    /// When the clock finished expiring this run's forge hold. A recorded
    /// expiry stops later ticks from acting again; manual resume retains the
    /// hold and checkpoints, but clears this run-local acknowledgement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forge_hold_expired_at: Option<DateTime<Utc>>,
}

impl PullDrainState {
    /// Replace the current pass while retaining the latest built forecast.
    pub fn record_admission_pass(&mut self, pass: DrainAdmissionPass) {
        let built = pass
            .admission_forecast
            .as_ref()
            .filter(|forecast| forecast.forecast_at.is_some());
        // Seed history from older checkpoints before an idle pass replaces it.
        let previous = self
            .drain_last_pass
            .as_ref()
            .and_then(|pass| pass.admission_forecast.as_ref())
            .filter(|forecast| forecast.forecast_at.is_some());
        if let Some(forecast) = built.or(previous) {
            self.drain_last_forecast = Some(forecast.clone());
        }
        self.drain_last_pass = Some(pass);
    }
}

/// A live operator request to stop a bounded drain's new admissions [ORB-11283].
///
/// This is not cancellation. The coordinator stays the same run, already
/// admitted children keep their completion authority, and the admission path
/// treats the flag as "offer nothing" on the next pass. Cancellation of those
/// children is a separate, explicit `orbit run cancel` of each child run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DrainAdmissionsStop {
    pub actor: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub stopped_at: DateTime<Utc>,
}

/// Why a follower cannot run a crew for the rest of its pull drain window, or
/// until a provider usage limit or a capacity backoff lifts.
#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum CrewExclusionSource {
    /// The window's provider preflight: the crew is disabled, or its
    /// provider CLI cannot be found or resolved here.
    Preflight,
    /// A claimed leaf failed because the provider could not be used.
    /// Authentication excludes every configured crew of that provider for
    /// the window; a capacity failure excludes only the leaf's crew, and only
    /// until the exclusion's `until`: a bounded backoff that doubles with each
    /// repeat in the window [ORB-15791].
    ProviderUnavailable,
    /// A claimed leaf on this crew was released for a failure class that
    /// [excludes its crew](crate::workflow::ClaimFailureClass::excludes_crew) [ORB-14257].
    LeafReleased,
    /// The host's latest reading of the crew's provider usage window is at
    /// or above the configured threshold, or exhausted. Like a capacity
    /// backoff it lasts only until the exclusion's `until`, so the crew is
    /// runnable again within the same window.
    ProviderLimit,
}

/// One crew a follower will not run, and why.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
pub struct CrewExclusion {
    pub crew: String,
    pub source: CrewExclusionSource,
    pub reason: String,
    /// When a timed exclusion lifts — a [`CrewExclusionSource::ProviderLimit`]
    /// or a capacity [`CrewExclusionSource::ProviderUnavailable`] — so the
    /// crew is runnable again in the same window; `None` for an exclusion
    /// that lasts the window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub until: Option<DateTime<Utc>>,
}

/// Operator-visible authentication exclusion for one provider on this host.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PullAuthExclusion {
    pub provider: String,
    pub host: String,
    pub excluded_at: DateTime<Utc>,
    pub error_class: String,
    pub relogin_hint: String,
    pub credential_source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_probe_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub attempts: u32,
}

impl PullAuthExclusion {
    /// Shared human-readable warning for doctor and run show.
    pub fn describe(&self) -> String {
        format!(
            "{} auth failed on {} at {} ({}; credentials: {}); {}; next probe: {}",
            self.provider,
            self.host,
            self.excluded_at.to_rfc3339(),
            self.error_class,
            self.credential_source,
            self.relogin_hint,
            self.next_probe_at.map_or_else(
                || "not declared; start a new drain after re-login".into(),
                |at| at.to_rfc3339()
            ),
        )
    }
}

/// Durable probe backoff and recovery for a released authentication incident.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PullAuthRecovery {
    pub exclusion: PullAuthExclusion,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovered_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_source: Option<String>,
}

/// The crews a pull drain's window can run, from the provider preflight it
/// took when the window opened [ORB-13941]. Cached for the window: a
/// newly installed CLI takes effect with the next drain. Auth failures may
/// recover within the window through a declared probe.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PullCrewPreflight {
    pub checked_at: DateTime<Utc>,
    /// Configured crews this host can run, by registry name.
    pub runnable: Vec<String>,
    /// The crew a task naming none runs as here (`workflow.default_crew`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_crew: Option<String>,
    /// Configured crews the preflight excluded.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub excluded: Vec<CrewExclusion>,
}

/// The one admission pass a pull drain submitted without a window is
/// authorized for [ORB-14174].
///
/// A pull drain with `for_seconds` zero runs one bounded admission pass and
/// then only settles. Its window is already expired by then, so the window
/// cannot tell that pass apart from a timed window that ran out; this marker
/// does. It is recorded before the pass sends its first request, so a retried
/// or resumed run (resume carries run state) sees the pass consumed and never
/// requests again, while the requests the pass recorded are still carried to
/// settlement.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PullSinglePass {
    pub consumed_at: DateTime<Utc>,
}

/// The pass that closed a timed pull drain's window for good [ORB-15417].
///
/// A pull drain keeps passing after its window lapses, until its leaves
/// settle, and an operator may extend the window during that tail. The pass
/// that finds the window still closed and nothing left to settle ends the
/// drain; it records this in the same transaction that rereads the deadline,
/// so an extension accepted first keeps the drain admitting and one requested
/// afterwards is refused instead of reported as accepted.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PullWindowClose {
    pub closed_at: DateTime<Utc>,
}

/// What a pull drain's host load forecast had room for when it last
/// requested work: each kind of work it would admit for its profile's whole
/// horizon, each it would not and why, and whether its request carried the
/// list. Shadow mode records it and requests as with `off`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PullAdmissionFit {
    /// `shadow` or `enforce`.
    pub mode: String,
    pub computed_at: DateTime<Utc>,
    /// Whether the request carried the list: under `enforce` only.
    pub sent: bool,
    /// The work that fits: `change/hard`, `no-diff/unset` or
    /// `auto-task:<template>`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub admissible: Vec<String>,
    /// The work that does not.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unfit: Vec<PullUnfitWork>,
}

/// Work a pull drain's forecast had no room for.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PullUnfitWork {
    /// As in [`PullAdmissionFit::admissible`].
    pub work: String,
    /// The first minute and resource over its limit, say `cpu 94% at +6m`.
    pub reason: String,
}

/// A pull drain's open failure breaker [ORB-15639]: the claims it admitted
/// last settled as consecutive failures, so it requests no new work until
/// `half_open_at`. From then it is half-open and admits one probe claim at a
/// time; a probe that succeeds closes it, and one that fails opens it again
/// with a longer wait. Absent while the breaker is closed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PullBreaker {
    /// The consecutive failures that opened it.
    pub failures: u32,
    /// Those failures' classes, newest first (`untyped` for a failure that
    /// carried none, such as an owner's refused bind).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failure_classes: Vec<String>,
    /// When the latest of them opened it.
    pub opened_at: DateTime<Utc>,
    /// When it half-opens to admit one probe claim.
    pub half_open_at: DateTime<Utc>,
}

impl PullBreaker {
    /// Whether it admits a probe claim at `now`.
    #[must_use]
    pub fn half_open(&self, now: DateTime<Utc>) -> bool {
        now >= self.half_open_at
    }

    /// `open after 3 consecutive failed claims (candidate, candidate,
    /// task_input) since 2026-10-11 16:53Z; half-opens at 2026-10-11 17:08Z to
    /// admit one probe claim`, or `half-open since ...` once that time passed.
    #[must_use]
    pub fn describe(&self, now: DateTime<Utc>) -> String {
        let classes = if self.failure_classes.is_empty() {
            String::new()
        } else {
            format!(" ({})", self.failure_classes.join(", "))
        };
        let half_open_at = self.half_open_at.format("%Y-%m-%d %H:%M:%SZ");
        let next = if self.half_open(now) {
            format!(
                "half-open since {half_open_at}: admits one probe claim once nothing this \
                 drain claimed is unsettled"
            )
        } else {
            format!("half-opens at {half_open_at} to admit one probe claim")
        };
        format!(
            "open after {} consecutive failed claims{classes} since {}; {next}",
            self.failures,
            self.opened_at.format("%Y-%m-%d %H:%M:%SZ"),
        )
    }
}

/// A graceful cancel of a pull drain that is waiting for its launched leaves.
///
/// The drain stops admitting at once and hands its unlaunched claims back to
/// the owner, but stays the same running coordinator until every leaf it
/// launched has finished and settled; only then does it end `cancelled`. A
/// forced cancel does not wait and records nothing here.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DrainCancelRequest {
    pub actor: String,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub requested_at: DateTime<Utc>,
}

/// Task disposition chosen by an operator cancellation. Kept in the run's
/// durable state before the owner is signalled so whichever process wins run
/// finalization applies the same task outcome and preserves the cancel note.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskCancellationPolicy {
    /// `true` preserves the legacy `blocked` transition; `false` returns the
    /// task to `backlog` so its candidate can be resumed later.
    pub block: bool,
    /// Actor and optional reason captured for the task history entry.
    pub note: String,
}

/// A live operator adjustment to a bounded drain's worker ceiling [ORB-11253].
///
/// The ceiling a drain was submitted with lives in its immutable
/// `initial_input`, which is why raising it used to mean cancelling the
/// coordinator and submitting a replacement. This is the mutable counterpart:
/// the admission path prefers it over the submitted value, so the same run id,
/// deadline, completion policy, and already-dispatched children survive the
/// change.
///
/// `revision` is the compare-and-set handle. Every accepted update increments
/// it, so a caller that read revision *n* and writes with `expected_revision`
/// *n* cannot silently overwrite an update that landed in between.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DrainWorkerLimit {
    /// Live ceiling on concurrently live leaf runs.
    pub max_active_leaf_runs: u32,
    /// The ceiling this update replaced, kept as change evidence.
    pub previous_max_active_leaf_runs: u32,
    /// Accepted-update counter, starting at 1.
    pub revision: u32,
    pub actor: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub updated_at: DateTime<Utc>,
}

/// A live operator extension of a bounded drain's window [ORB-15383].
///
/// A drain stamps its deadline once, in its `open_window` step output, and
/// every later window answer echoes that stamp. This is the mutable
/// counterpart, like [`DrainWorkerLimit`] is for the ceiling: the window
/// answer, readiness and upgrade-resume all read the later of the stamp and
/// this deadline, so the run keeps its id, its children and its authority
/// while its window grows. It never shortens a window and never reopens a
/// drain whose admissions were stopped.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DrainWindowExtension {
    /// The deadline in force since this extension.
    pub deadline: DateTime<Utc>,
    /// The deadline it replaced, kept as change evidence.
    pub previous_deadline: DateTime<Utc>,
    /// Accepted-extension counter, starting at 1.
    pub revision: u32,
    pub actor: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub extended_at: DateTime<Utc>,
}

/// A backlog task a drain's admission pass left unstarted, and why.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DrainWaitingTask {
    pub task_id: String,
    /// Why the drain could not admit it (`context_lock_conflict`,
    /// `crew_not_allowed`, `host_os_mismatch`, ...); absent for a plain lock
    /// deferral.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The tasks holding what it needs, when known.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked_by: Vec<String>,
    /// What clears the wait, when the reason alone does not say: the host OS
    /// a `host_os_mismatch` task waits for, say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// What an `--approve-proposed` drain has done with `proposed` work.
///
/// The approval steps write it on the drain's own run state: the selection
/// step replaces the held view every pass, and the record step in the pass
/// that sees a task-pilot child finish adds the tasks that child approved,
/// closing the child's dispatch so it is counted once. Like `drain_last_pass` it
/// survives terminalization, so `run show` reports a finished drain's
/// approvals and holds.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct DrainApprovalReport {
    pub recorded_at: Option<DateTime<Utc>>,
    /// Proposed tasks this drain moved to `backlog` over its whole window.
    #[serde(default)]
    pub approved_total: u64,
    /// The most recent of those (bounded list).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub approved: Vec<String>,
    /// Proposed tasks Orbit automation filed that this drain archived because
    /// task-pilot verified them already fixed by a commit on the base branch.
    #[serde(default)]
    pub closed_total: u64,
    /// The most recent of those (bounded list).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub closed: Vec<String>,
    /// Proposed tasks the latest pass left proposed, with the reason: the
    /// qualification they miss, or the task-pilot classification that held
    /// them (bounded list).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub held: Vec<DrainWaitingTask>,
    /// The full count behind `held`.
    #[serde(default)]
    pub held_total: u64,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub held_by_reason: BTreeMap<String, u64>,
}

/// Workspace capacity observed before a local drain's latest admission wave.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DrainCapacity {
    /// Shared slots occupied by live deliveries and unsettled pull admissions.
    pub active_leaf_runs: u64,
    /// Occupied slots outside this coordinator's dispatch lineage or claims.
    pub inherited_leaf_runs: u64,
    /// Effective ceiling, including an operator's live resize.
    pub max_active_leaf_runs: u64,
}

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
    /// [ORB-15637] Why this pull pass requested no new claim although its
    /// window was open: the owner's probe refusal, a refused settlement
    /// (`settlement_refused`), the failure breaker (`circuit_open`), a host
    /// shutdown inside the hold horizon (`host_shutdown_scheduled`), or no
    /// runnable crew (`no_runnable_crew`, `host_suppressed`). Set afresh each
    /// pass, never carried. Host pressure and degradation have their own
    /// fields. Pull drains only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admission_refusal: Option<String>,
    /// The latest failed pull pass, or a local pass deferred on a busy lock
    /// (naming the holder); cleared by success before degradation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_pass_error: Option<String>,
    /// Consecutive failed pull passes, independent of failed claimed leaves,
    /// or consecutive deferred local passes; a local drain fails at ten.
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
    /// `cpu`, `memory`, or `disk <path>`.
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

    /// The operator-facing hold sentence every surface prints.
    #[must_use]
    pub fn hold_reason(&self) -> String {
        format!(
            "Admissions throttled: {}. Running work is not touched.",
            self.describe()
        )
    }
}
