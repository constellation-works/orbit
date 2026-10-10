//! Persistent pipeline state for a job run.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::workflow::JobRunState;
use crate::workflow::JobRunTrigger;
use crate::workflow::child_dispatch::{
    ChildCancellation, ChildCancellationPolicy, ChildDispatch, ChildDispatchPhase,
};

use super::{
    ActivityCrewDraw, DrainAdmissionPass, DrainAdmissionsStop, DrainApprovalReport,
    DrainCancelRequest, DrainWorkerLimit, FailureActivityCheckpoint, FinalRecoveryCheckpoint,
    PullAuthRecovery, PullCrewPreflight, PullSinglePass, TaskCancellationPolicy,
};

/// The PR leaf step that admits the before-landing review of its open pull
/// request [ORB-14849]; its output's `applies` says whether that review runs.
const LANDING_REVIEW_ADMIT_STEP: &str = "landing_review_gate_admit";

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
    /// These are used to rebuild `steps.*` template context during recovery;
    /// a finished run drops those its `pipeline` also holds (see
    /// `step_output_pointers`), so read a step's output through
    /// [`Self::step_output`].
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
    /// Where a finished run's step outputs live in `pipeline`, as RFC 6901
    /// JSON pointers keyed by global step index. A run that ends `success` or
    /// `cancelled` can never resume, so [`Self::compact_for_terminal`] drops
    /// the resume-only maps and records here where each dropped output is
    /// still found; [`Self::step_output`] reads either form. An output the
    /// pipeline does not hold stays in `step_outputs`: it is the only copy.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub step_output_pointers: BTreeMap<u32, String>,
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
    /// Operator-selected task disposition for this run's cancellation. Missing
    /// on older or non-operator terminalizations, which retain failure blocking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_cancellation_policy: Option<TaskCancellationPolicy>,
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
    /// What this drain's last admission pass left waiting. Survives
    /// terminalization: it is how `run show` reports the backlog a finished
    /// drain never started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drain_last_pass: Option<DrainAdmissionPass>,
    /// What this `--approve-proposed` drain approved and held. Survives
    /// terminalization like `drain_last_pass`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drain_approvals: Option<DrainApprovalReport>,
    /// Successful terminal failure activity output, when one ran.
    ///
    /// This remains distinct from the successful-step maps: the original step
    /// failure stays authoritative while a resume can still authenticate any
    /// candidate the failure activity preserved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_activity_checkpoint: Option<FailureActivityCheckpoint>,
    /// Rebase completions keyed by failed step ID: the latest recovery attempt
    /// of each step, whose payload names its host-assigned
    /// `recovery_attempt`. These are provenance, not successful step outputs;
    /// retries must still run the step. Absent in older runs, whose rewritten
    /// heads remain unverified.
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
    /// How this run was submitted [ORB-12255]. Absent on runs recorded before
    /// trigger provenance existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<JobRunTrigger>,
    /// `execution.env.pass` names the submitting process held no value for
    /// when this run was submitted [ORB-14777]: agents it starts do not
    /// receive them. Names only, never values. Absent when none were unset
    /// and on runs recorded before this existed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env_pass_unset: Vec<String>,
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
            step_output_pointers: BTreeMap::new(),
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
            task_cancellation_policy: None,
            pull_crew_preflight: None,
            pull_auth_recovery: BTreeMap::new(),
            pull_single_pass: None,
            drain_last_pass: None,
            drain_approvals: None,
            failure_activity_checkpoint: None,
            rebase_recovery_checkpoints: BTreeMap::new(),
            activity_crew_draws: BTreeMap::new(),
            final_recovery: None,
            forge_hold: None,
            forge_hold_expired_at: None,
            trigger: None,
            env_pass_unset: Vec::new(),
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

    /// The output step `step_index` recorded: its resume checkpoint while the
    /// run can still resume, its entry in `pipeline` once compacted.
    pub fn step_output(&self, step_index: u32) -> Option<&Value> {
        self.step_outputs.get(&step_index).or_else(|| {
            self.step_output_pointers
                .get(&step_index)
                .and_then(|pointer| self.pipeline.pointer(pointer))
        })
    }

    /// The output the step named `step_id` checkpointed into `pipeline`.
    ///
    /// Unlike [`Self::step_output`] this needs no step index, so a reader that
    /// only knows the step's id (the audit trail names steps by id and numbers
    /// them in first-started order, not by YAML position) cannot land on
    /// another step's checkpoint. A step that was skipped, or has not
    /// completed, has no entry.
    pub fn pipeline_step_output(&self, step_id: &str) -> Option<&Value> {
        self.pipeline.get(step_id)
    }

    /// Whether this PR leaf has reached its before-landing review [ORB-15194]:
    /// the admission step after `pr_open` recorded that the review applies.
    /// It stays true through the review, its revalidation and the handoff
    /// that follow, so a reader pairs it with a live run state.
    pub fn in_landing_review(&self) -> bool {
        self.pipeline_step_output(LANDING_REVIEW_ADMIT_STEP)
            .and_then(|output| output.get("applies"))
            .and_then(Value::as_bool)
            == Some(true)
    }

    /// Every recorded step output in step order, compacted or not.
    pub fn step_output_entries(&self) -> impl DoubleEndedIterator<Item = (u32, &Value)> {
        let indices: std::collections::BTreeSet<u32> = self
            .step_outputs
            .keys()
            .chain(self.step_output_pointers.keys())
            .copied()
            .collect();
        indices
            .into_iter()
            .filter_map(|index| self.step_output(index).map(|output| (index, output)))
    }

    /// Drop what only resume reads once `run_state` rules resume out.
    ///
    /// `success` and `cancelled` are the terminals no resume accepts; every
    /// other state keeps the full checkpoint. Each step output the `pipeline`
    /// already holds is replaced by a pointer to that entry; checkpoints
    /// write every output there, so normally none remain. Returns whether
    /// anything was dropped. Idempotent.
    pub fn compact_for_terminal(&mut self, run_state: JobRunState) -> bool {
        if !matches!(run_state, JobRunState::Success | JobRunState::Cancelled) {
            return false;
        }
        let mut compacted = !self.pipeline_patches.is_empty() || !self.compound_outputs.is_empty();
        let pipeline = &self.pipeline;
        let pointers = &mut self.step_output_pointers;
        self.step_outputs.retain(|index, output| {
            let Some(pointer) = pipeline_pointer_to(pipeline, output) else {
                return true;
            };
            pointers.insert(*index, pointer);
            compacted = true;
            false
        });
        self.pipeline_patches.clear();
        self.compound_outputs.clear();
        compacted
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

/// The JSON pointer to the first `pipeline` entry equal to `output`: the
/// whole document, else a top-level key. Checkpoints write each step output
/// under its step id, so a top-level search finds every output still held.
fn pipeline_pointer_to(pipeline: &Value, output: &Value) -> Option<String> {
    if pipeline == output {
        return Some(String::new());
    }
    pipeline
        .as_object()?
        .iter()
        .find(|(_, value)| *value == output)
        .map(|(key, _)| format!("/{}", key.replace('~', "~0").replace('/', "~1")))
}

fn merge_pipeline_patch(pipeline: &mut Value, patch: &Value) {
    if let (Some(pipeline_map), Some(patch_map)) = (pipeline.as_object_mut(), patch.as_object()) {
        for (key, value) in patch_map {
            pipeline_map.insert(key.clone(), value.clone());
        }
    }
}
