use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::task::TaskStatus;
use crate::workflow::JobRunState;
use crate::workflow::JobRunTrigger;
use crate::workflow::child_dispatch::{
    ChildCancellation, ChildCancellationPolicy, ChildDispatch, ChildDispatchPhase,
};
use crate::workflow::final_recovery::FinalRecoveryDecision;

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

/// Token a provider failure diagnostic carries when the provider itself could
/// not be used on this host — its CLI refused authentication, say — as
/// opposed to the agent failing the work [ORB-13941].
///
/// The CLI runner stamps it, bracketed, into the step's failure message; a
/// pull drain reads it back off the terminal leaf to release the claim and
/// exclude the crew for its window instead of failing the owner's task.
pub const PROVIDER_UNAVAILABLE_ERROR_CODE: &str = "provider_unavailable";

/// The bracketed marker form of [`PROVIDER_UNAVAILABLE_ERROR_CODE`].
pub const PROVIDER_UNAVAILABLE_MARKER: &str = "[provider_unavailable]";

/// Whether a step failure says its provider could not be used on this host.
#[must_use]
pub fn is_provider_unavailable(error_code: Option<&str>, message: Option<&str>) -> bool {
    error_code == Some(PROVIDER_UNAVAILABLE_ERROR_CODE)
        || message.is_some_and(|message| message.contains(PROVIDER_UNAVAILABLE_MARKER))
}

/// Why a follower cannot run a crew for the rest of its pull drain window.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum CrewExclusionSource {
    /// The window's provider preflight: the crew is disabled, or its
    /// provider CLI cannot be found or resolved here.
    Preflight,
    /// A claimed leaf on this crew failed because the provider could not be
    /// used (an authentication failure, for instance).
    ProviderUnavailable,
}

/// One crew a follower will not run, and why.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CrewExclusion {
    pub crew: String,
    pub source: CrewExclusionSource,
    pub reason: String,
}

/// The crews a pull drain's window can run, from the provider preflight it
/// took when the window opened [ORB-13941]. Cached for the window: a fixed
/// credential or newly installed CLI takes effect with the next drain.
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

/// A backlog task a drain's admission pass left unstarted, and why.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DrainWaitingTask {
    pub task_id: String,
    /// Why the drain could not admit it (`context_lock_conflict`,
    /// `crew_not_allowed`, ...); absent for a plain lock deferral.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The tasks holding what it needs, when known.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked_by: Vec<String>,
}

/// What a drain's most recent admission pass left waiting.
///
/// The classifier's own output lives only inside the running loop, so this is
/// the one durable record of the backlog a finished drain never started. It is
/// overwritten every pass: the last one is the drain's final view.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DrainAdmissionPass {
    pub recorded_at: DateTime<Utc>,
    /// Admissible tasks the pass did not admit: no free slot, or a lock
    /// conflict.
    pub queued: u64,
    /// The subset of `queued` a lock conflict kept out.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deferred: Vec<DrainWaitingTask>,
    /// Backlog tasks the drain could not admit at all (bounded list).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub excluded: Vec<DrainWaitingTask>,
    /// The full count behind `excluded`.
    #[serde(default)]
    pub excluded_total: u64,
    /// Host resource pressure that held this pass's admissions [ORB-13901].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_throttle: Option<ResourceThrottle>,
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
    /// `memory 93% ≥ 90% since 08:41Z; cpu 97% ≥ 90% since 08:40Z`.
    #[must_use]
    pub fn describe(&self) -> String {
        self.resources
            .iter()
            .map(|pressure| {
                format!(
                    "{} {:.0}% \u{2265} {}% since {}",
                    pressure.resource,
                    pressure.percent,
                    pressure.high_percent,
                    pressure.since.format("%Y-%m-%d %H:%MZ"),
                )
            })
            .collect::<Vec<_>>()
            .join("; ")
    }

    /// The operator-facing hold sentence every surface prints.
    #[must_use]
    pub fn hold_reason(&self) -> String {
        let resume = self
            .resources
            .iter()
            .map(|pressure| format!("{} below {}%", pressure.resource, pressure.resume_percent))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "Admissions throttled: {}. New tasks start again once {resume}; running work is not \
             touched.",
            self.describe()
        )
    }
}

/// Durable result of a job-level terminal failure activity.
///
/// A failure activity is not a successful workflow step, so its output cannot
/// live in `step_outputs`. Keeping it separately preserves the evidence needed
/// to resume from a recovery action without treating the failed step as
/// completed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FailureActivityCheckpoint {
    pub activity_name: String,
    pub failed_step_id: String,
    pub output: Value,
}

/// One weighted member of the pool an activity crew draw ran on.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActivityCrewPoolMember {
    pub name: String,
    pub weight: u32,
}

/// A crew an activity drew from a configured pool, frozen for the run.
///
/// An activity whose crew comes from a weighted pool (its `crew_config_key`)
/// draws once; every later dispatch of that key in the run, and every resume
/// seeded from it, reuses this record instead of rerolling.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActivityCrewDraw {
    /// Canonical crew name the draw selected.
    pub crew: String,
    /// The configuration the pool came from (`workflow.final_recovery_crews`).
    pub source: String,
    /// The permitted members and weights the draw ran on, so `run show`
    /// can explain the choice.
    pub eligible_pool: Vec<ActivityCrewPoolMember>,
}

/// The task revision final recovery observed when it was admitted.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct FinalRecoveryObservedTask {
    pub status: TaskStatus,
    pub updated_at: DateTime<Utc>,
}

/// Idempotency key of a run's final-recovery decision: the run that admitted
/// it and that run's attempt. The applier names the run in the task comment
/// it writes, and a decision whose run already has that comment is not
/// applied again.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FinalRecoveryKey {
    pub run_id: String,
    pub attempt: u32,
}

/// A run's job-level final recovery [ORB-13907], recorded when it is admitted.
///
/// Admission writes this before the activity is dispatched, so a crash during
/// the activity, a resume decision, or an operator resume seeded from this
/// state never dispatches final recovery for the run a second time. The
/// decision is recorded here before it touches the task. A run resumed from
/// state that holds a decision other than `resume` applies that decision again
/// instead of dispatching, so a crash at any point converges on one outcome.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FinalRecoveryCheckpoint {
    pub key: FinalRecoveryKey,
    /// Top-level step whose failure admitted final recovery.
    pub failed_step_id: String,
    pub task_id: String,
    /// The task as it stood at admission; the applier refuses a decision once
    /// the task has changed since. Absent for a claimed leaf, whose task lives
    /// on its owner and is re-read there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed: Option<FinalRecoveryObservedTask>,
    /// Base ref a `complete_no_diff` commit must be reachable from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_ref: Option<String>,
    pub admitted_at: DateTime<Utc>,
    /// The decision acted on, recorded before it is applied. The engine's
    /// substitutions (an invalid resume step, a failed activity) are recorded
    /// as the `escalate` they became.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<FinalRecoveryDecision>,
    /// What applying the decision did (`resume`, `settled: …` or
    /// `escalated: …`); absent while the decision is only intended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
}

/// Persistent pipeline state for a job run.
///
/// Stored as `state.json` in the run bundle directory. Steps read accumulated
/// state from `pipeline` and write their recovery metadata back so retry and
/// reconcile can resume from the persisted snapshot.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PipelineState {
    pub run_id: String,
    pub job_id: String,
    /// Merged job defaults + run input. Immutable after creation.
    pub initial_input: Value,
    /// Accumulated pipeline state — each step's output is merged here.
    /// This replaces the in-memory `current_input` blob.
    pub pipeline: Value,
    /// Raw per-step outputs keyed by global step index.
    /// These are used to rebuild `steps.*` template context during recovery.
    #[serde(default)]
    pub step_outputs: BTreeMap<u32, Value>,
    /// Pipeline entries a completed compound step (`parallel:`, `fan_out:`,
    /// `loop:`) exposed besides its own output — nested step outputs and
    /// nested fan-in `collect` aliases — keyed by global step index, then by
    /// pipeline key. Resume restores them alongside `step_outputs` so later
    /// steps see the same `steps.*` entries as an uninterrupted run. Absent
    /// in checkpoints recorded before this field existed; those restore only
    /// the step's own output (and a top-level fan-in alias, which equals it).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub compound_outputs: BTreeMap<u32, BTreeMap<String, Value>>,
    /// Per-step pipeline patches keyed by global step index.
    /// Successful steps merge these patches into `pipeline`.
    #[serde(default)]
    pub pipeline_patches: BTreeMap<u32, Value>,
    /// Per-step states keyed by global step index.
    #[serde(default)]
    pub step_states: BTreeMap<u32, JobRunState>,
    /// Next global step index the engine should execute.
    #[serde(default)]
    pub next_step_index: u32,
    /// Last non-skipped step state observed by the run.
    #[serde(default)]
    pub previous_step_state: Option<JobRunState>,
    /// Current loop iteration (0-based). Updated at each loop boundary.
    #[serde(default)]
    pub iteration: u32,
    /// Task dependencies currently blocking this run, when the run is parked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting_on_deps: Option<Vec<String>>,
    /// Task lock resource identifiers currently blocking this run, when parked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting_on_locks: Option<Vec<String>>,
    /// Child Runs this run dispatched, in submission order.
    ///
    /// For auto children, written in the same durable admission transaction
    /// that creates the child run [ORB-11310]. Other child callers checkpoint
    /// it the moment `orbit.pipeline.invoke` returns. Parent/child lineage is
    /// therefore observable before a blocking wait and survives
    /// terminalization: a cancelled parent must still name the child it left
    /// behind.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub child_dispatches: Vec<ChildDispatch>,
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
    /// A graceful cancel this pull drain is carrying out: present while it
    /// waits for its launched leaves, and kept once it ends `cancelled`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drain_cancel: Option<DrainCancelRequest>,
    /// The provider preflight a pull drain took when its window opened.
    /// Survives terminalization: it is how `run show` reports which crews
    /// the drain could not run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_crew_preflight: Option<PullCrewPreflight>,
    /// What this drain's last admission pass left waiting. Survives
    /// terminalization: it is how `run show` reports the backlog a finished
    /// drain never started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drain_last_pass: Option<DrainAdmissionPass>,
    /// Successful terminal failure activity output, when one ran.
    ///
    /// This remains distinct from the successful-step maps: the original step
    /// failure stays authoritative while a resume can still authenticate any
    /// candidate the failure activity preserved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_activity_checkpoint: Option<FailureActivityCheckpoint>,
    /// Rebase completions keyed by failed step ID. These are provenance, not
    /// successful step outputs; retries must still run the step. Absent in
    /// older runs, whose rewritten heads remain unverified.
    ///
    /// This map is untrusted progress data. Managed leaves hold modify grants
    /// on the store it is persisted in, so a matching entry is a candidate
    /// only: authority is the host-only certificate a resume checks through
    /// `RuntimeHost::verify_rebase_recovery`. Never treat an entry here as
    /// evidence on its own.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub rebase_recovery_checkpoints: BTreeMap<String, Value>,
    /// Crews drawn from configured pools for activities, keyed by the
    /// activity's `crew_config_key`. Written once per key; resume carries it
    /// with the rest of the state so a resumed run keeps the same crew.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub activity_crew_draws: BTreeMap<String, ActivityCrewDraw>,
    /// This run's final recovery [ORB-13907]; present once it was admitted.
    /// Resume clones it, so a resumed run never invokes final recovery again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_recovery: Option<FinalRecoveryCheckpoint>,
    /// How this run was submitted [ORB-12255]. Absent on runs recorded before
    /// trigger provenance existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<JobRunTrigger>,
    pub updated_at: DateTime<Utc>,
}

impl PipelineState {
    /// Create a new pipeline state from initial inputs.
    pub fn new(run_id: String, job_id: String, initial_input: Value) -> Self {
        Self {
            run_id,
            job_id,
            pipeline: initial_input.clone(),
            initial_input,
            step_outputs: BTreeMap::new(),
            compound_outputs: BTreeMap::new(),
            pipeline_patches: BTreeMap::new(),
            step_states: BTreeMap::new(),
            next_step_index: 0,
            previous_step_state: None,
            iteration: 0,
            waiting_on_deps: None,
            waiting_on_locks: None,
            child_dispatches: Vec::new(),
            drain_worker_limit: None,
            drain_admissions_stop: None,
            drain_cancel: None,
            pull_crew_preflight: None,
            drain_last_pass: None,
            failure_activity_checkpoint: None,
            rebase_recovery_checkpoints: BTreeMap::new(),
            activity_crew_draws: BTreeMap::new(),
            final_recovery: None,
            trigger: None,
            updated_at: Utc::now(),
        }
    }

    /// The compare-and-set handle for [`Self::drain_worker_limit`]. Zero means
    /// no operator adjustment has been accepted yet.
    pub fn drain_worker_limit_revision(&self) -> u32 {
        self.drain_worker_limit
            .as_ref()
            .map_or(0, |limit| limit.revision)
    }

    /// The ceiling currently in force, given the value the run was submitted
    /// with. An operator adjustment always wins over the submitted input:
    /// it is the more recent statement of the same decision.
    pub fn effective_max_active_leaf_runs(&self, submitted: u32) -> u32 {
        self.drain_worker_limit
            .as_ref()
            .map_or(submitted, |limit| limit.max_active_leaf_runs)
    }

    /// Record an accepted worker-ceiling change, replacing `submitted` when no
    /// adjustment is recorded yet.
    ///
    /// Returns `false` and mutates nothing when `expected_revision` names a
    /// revision other than the persisted one — the caller read a ceiling that
    /// another operator has since replaced, and applying its arithmetic anyway
    /// would silently discard that update.
    pub fn set_drain_worker_limit(
        &mut self,
        max_active_leaf_runs: u32,
        submitted: u32,
        actor: String,
        reason: Option<String>,
        expected_revision: Option<u32>,
    ) -> bool {
        let revision = self.drain_worker_limit_revision();
        if expected_revision.is_some_and(|expected| expected != revision) {
            return false;
        }
        self.drain_worker_limit = Some(DrainWorkerLimit {
            max_active_leaf_runs,
            previous_max_active_leaf_runs: self.effective_max_active_leaf_runs(submitted),
            revision: revision.saturating_add(1),
            actor,
            reason,
            updated_at: Utc::now(),
        });
        self.updated_at = Utc::now();
        true
    }

    /// Whether this drain has been told to stop offering new work.
    pub fn admissions_stopped(&self) -> bool {
        self.drain_admissions_stop.is_some()
    }

    /// Record an admissions stop. Idempotent: a drain that is already stopped
    /// keeps the original actor and timestamp and returns `false`.
    pub fn set_drain_admissions_stop(&mut self, actor: String, reason: Option<String>) -> bool {
        if self.drain_admissions_stop.is_some() {
            return false;
        }
        self.drain_admissions_stop = Some(DrainAdmissionsStop {
            actor,
            reason,
            stopped_at: Utc::now(),
        });
        self.updated_at = Utc::now();
        true
    }

    /// Whether a graceful cancel is waiting for this drain's leaves.
    pub fn drain_cancelling(&self) -> bool {
        self.drain_cancel.is_some()
    }

    /// Record a graceful cancel, stopping new admissions with it. Idempotent:
    /// a drain already cancelling keeps the first request and returns `false`.
    pub fn set_drain_cancel(
        &mut self,
        actor: String,
        source: String,
        reason: Option<String>,
    ) -> bool {
        if self.drain_cancel.is_some() {
            return false;
        }
        self.set_drain_admissions_stop(actor.clone(), reason.clone());
        self.drain_cancel = Some(DrainCancelRequest {
            actor,
            source,
            reason,
            requested_at: Utc::now(),
        });
        self.updated_at = Utc::now();
        true
    }

    /// Preserve the successful terminal failure activity without converting
    /// the failed workflow step into a completed checkpoint.
    pub fn record_failure_activity(
        &mut self,
        activity_name: String,
        failed_step_id: String,
        output: Value,
    ) {
        self.failure_activity_checkpoint = Some(FailureActivityCheckpoint {
            activity_name,
            failed_step_id,
            output,
        });
        self.updated_at = Utc::now();
    }

    /// Record step recovery metadata and advance the resume cursor.
    pub fn record_step(
        &mut self,
        step_index: u32,
        step_state: JobRunState,
        raw_output: Option<Value>,
        pipeline_patch: Option<Value>,
    ) {
        if let Some(output) = raw_output {
            self.step_outputs.insert(step_index, output);
        }
        if step_state == JobRunState::Success
            && let Some(patch) = pipeline_patch
        {
            merge_pipeline_patch(&mut self.pipeline, &patch);
            self.pipeline_patches.insert(step_index, patch);
        }
        self.step_states.insert(step_index, step_state);
        if step_state != JobRunState::Skipped {
            self.previous_step_state = Some(step_state);
        }
        self.next_step_index = step_index.saturating_add(1);
        self.updated_at = Utc::now();
    }

    /// Replace the nested pipeline entries recorded for `step_index`; an
    /// empty map clears them so a re-checkpointed step never keeps entries
    /// from an earlier attempt.
    pub fn record_compound_outputs(&mut self, step_index: u32, outputs: BTreeMap<String, Value>) {
        if outputs.is_empty() {
            self.compound_outputs.remove(&step_index);
        } else {
            self.compound_outputs.insert(step_index, outputs);
        }
        self.updated_at = Utc::now();
    }

    /// Record one completed step's output under its step id in the
    /// accumulated pipeline, replacing a non-object pipeline value.
    pub fn record_pipeline_output(&mut self, step_id: &str, output: Value) {
        if !self.pipeline.is_object() {
            self.pipeline = Value::Object(Default::default());
        }
        if let Some(pipeline) = self.pipeline.as_object_mut() {
            pipeline.insert(step_id.to_string(), output);
        }
        self.updated_at = Utc::now();
    }

    /// Replace the accumulated pipeline snapshot directly.
    pub fn sync_pipeline(&mut self, pipeline: Value) {
        self.pipeline = pipeline;
        self.updated_at = Utc::now();
    }

    pub fn set_waiting_reasons(
        &mut self,
        waiting_on_deps: Option<Vec<String>>,
        waiting_on_locks: Option<Vec<String>>,
    ) {
        self.waiting_on_deps = waiting_on_deps;
        self.waiting_on_locks = waiting_on_locks;
        self.updated_at = Utc::now();
    }

    pub fn clear_waiting_reasons(&mut self) {
        self.waiting_on_deps = None;
        self.waiting_on_locks = None;
        self.updated_at = Utc::now();
    }

    /// Record a child dispatch, keyed by the child's run id.
    ///
    /// Upsert rather than push: a resumed or retried parent re-executing the
    /// same dispatch step must not accumulate duplicate rows for one child.
    /// A re-record keeps the original `submitted_at` so the observable
    /// submission instant does not drift.
    ///
    /// A dispatch that already terminalized is left untouched: a late refresh
    /// from a parent that was cancelled mid-dispatch must not reopen the link
    /// or erase its recorded status, error, or cancellation.
    pub fn record_child_dispatch(&mut self, dispatch: ChildDispatch) {
        match self
            .child_dispatches
            .iter_mut()
            .find(|existing| existing.child_run_id == dispatch.child_run_id)
        {
            Some(existing) if !existing.phase.is_open() => return,
            Some(existing) => {
                let submitted_at = existing.submitted_at;
                *existing = dispatch;
                existing.submitted_at = submitted_at;
            }
            None => self.child_dispatches.push(dispatch),
        }
        self.updated_at = Utc::now();
    }

    /// Advance a recorded child dispatch. Returns false when no dispatch with
    /// that child run id is recorded, so a caller can tell a lost checkpoint
    /// from a successful update instead of silently succeeding.
    ///
    /// A terminal dispatch never reopens: a late `Submitted` or `Waiting`
    /// write is ignored. A late terminal observation only fills in a status or
    /// error that is still missing, so the parent's view of how the child
    /// ended survives without overwriting evidence already recorded.
    pub fn advance_child_dispatch(
        &mut self,
        child_run_id: &str,
        phase: ChildDispatchPhase,
        child_status: Option<String>,
        error: Option<String>,
    ) -> bool {
        let Some(dispatch) = self
            .child_dispatches
            .iter_mut()
            .find(|dispatch| dispatch.child_run_id == child_run_id)
        else {
            return false;
        };
        if dispatch.phase.is_open() {
            dispatch.phase = phase;
            if child_status.is_some() {
                dispatch.child_status = child_status;
            }
            if error.is_some() {
                dispatch.error = error;
            }
        } else if phase.is_open() {
            return true;
        } else {
            if dispatch.child_status.is_none() {
                dispatch.child_status = child_status;
            }
            if dispatch.error.is_none() {
                dispatch.error = error;
            }
        }
        dispatch.updated_at = Utc::now();
        self.updated_at = Utc::now();
        true
    }

    /// Every child dispatch the parent still considers open.
    pub fn open_child_dispatches(&self) -> impl Iterator<Item = &ChildDispatch> {
        self.child_dispatches
            .iter()
            .filter(|dispatch| dispatch.phase.is_open())
    }

    /// Close an open dispatch because the parent itself terminalized, and
    /// record which cancellation policy was applied to the child.
    ///
    /// The linkage itself is never dropped: an operator who cancels a parent
    /// mid-wait still needs the child run id, which is the only handle on the
    /// work that outlived (or was stopped with) the parent.
    pub fn terminalize_child_dispatch(
        &mut self,
        child_run_id: &str,
        cancellation: ChildCancellation,
    ) -> bool {
        let Some(dispatch) = self
            .child_dispatches
            .iter_mut()
            .find(|dispatch| dispatch.child_run_id == child_run_id)
        else {
            return false;
        };
        dispatch.phase = ChildDispatchPhase::Terminal;
        dispatch.cancellation = Some(cancellation);
        dispatch.updated_at = Utc::now();
        self.updated_at = Utc::now();
        true
    }

    /// The child run ids a terminalizing parent must cancel, per each
    /// dispatch's own [`ChildCancellationPolicy`].
    pub fn cascade_cancellation_targets(&self) -> Vec<String> {
        self.open_child_dispatches()
            .filter(|dispatch| dispatch.cancellation_policy() == ChildCancellationPolicy::Cascade)
            .map(|dispatch| dispatch.child_run_id.clone())
            .collect()
    }
}

fn merge_pipeline_patch(pipeline: &mut Value, patch: &Value) {
    if let (Some(pipeline_map), Some(patch_map)) = (pipeline.as_object_mut(), patch.as_object()) {
        for (key, value) in patch_map {
            pipeline_map.insert(key.clone(), value.clone());
        }
    }
}
