use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::workflow::JobRunState;
use crate::workflow::child_dispatch::{
    ChildCancellation, ChildCancellationPolicy, ChildDispatch, ChildDispatchPhase,
};

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
    /// Successful terminal failure activity output, when one ran.
    ///
    /// This remains distinct from the successful-step maps: the original step
    /// failure stays authoritative while a resume can still authenticate any
    /// candidate the failure activity preserved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_activity_checkpoint: Option<FailureActivityCheckpoint>,
    /// Host-validated rebase completions keyed by failed step ID. These are
    /// provenance, not successful step outputs; retries must still run the step.
    /// Absent in older runs, whose rewritten heads remain unverified.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub rebase_recovery_checkpoints: BTreeMap<String, Value>,
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
            failure_activity_checkpoint: None,
            rebase_recovery_checkpoints: BTreeMap::new(),
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

    /// Replace the accumulated pipeline snapshot directly.
    pub fn sync_pipeline(&mut self, pipeline: Value) {
        self.pipeline = pipeline;
        self.updated_at = Utc::now();
    }

    pub fn set_iteration(&mut self, iteration: u32) {
        self.iteration = iteration;
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
    pub fn record_child_dispatch(&mut self, dispatch: ChildDispatch) {
        match self
            .child_dispatches
            .iter_mut()
            .find(|existing| existing.child_run_id == dispatch.child_run_id)
        {
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
        dispatch.phase = phase;
        if child_status.is_some() {
            dispatch.child_status = child_status;
        }
        if error.is_some() {
            dispatch.error = error;
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

    /// Rebuild the pipeline snapshot just before `step_index` executes.
    pub fn rebuild_pipeline_before(&self, step_index: u32) -> Value {
        let mut pipeline = self.initial_input.clone();
        for (_, patch) in self.pipeline_patches.range(..step_index) {
            merge_pipeline_patch(&mut pipeline, patch);
        }
        pipeline
    }

    /// Recover the last non-skipped step state before `step_index`.
    pub fn previous_step_state_before(&self, step_index: u32) -> Option<JobRunState> {
        self.step_states
            .range(..step_index)
            .rev()
            .map(|(_, state)| *state)
            .find(|state| *state != JobRunState::Skipped)
    }
}

fn merge_pipeline_patch(pipeline: &mut Value, patch: &Value) {
    if let (Some(pipeline_map), Some(patch_map)) = (pipeline.as_object_mut(), patch.as_object()) {
        for (key, value) in patch_map {
            pipeline_map.insert(key.clone(), value.clone());
        }
    }
}
