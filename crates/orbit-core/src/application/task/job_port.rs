//! What task writes and task-owned steps need from job dispatch and the
//! crew pools [ORB-17936].
//!
//! Task creation and re-rating draw a crew, provider-hold and provider-limit
//! admission narrow a task's candidates within the pools its admitting
//! pipeline captured, task-pilot preparation asks whether the PR route would
//! find a forge, light delivery reads the candidate's validation gates, the
//! baseline-hold refresh and the blocked-task backstop submit their routine
//! runs, completion and requeue ask whether a run's owner is still
//! executing, and requeue asks whether an owner publication is unbound. `job` owns all of that, and it also drives task
//! writes, so a direct call from here would close a cycle. This port is the
//! way in: `job` implements it on [`OrbitRuntime`](crate::OrbitRuntime), so
//! composing the runtime binds it. The values both sides name live in
//! [`contracts`](crate::application::contracts).

use orbit_common::OrbitError;
use orbit_types::identity::Crew;
use orbit_types::task::{Task, TaskComplexity, TaskHistoryEntry};
use orbit_types::workflow::{
    HostValidationGates, JobRun, JobRunTrigger, PipelineState, RunDeliveryObservation, ShipMode,
    SourceRepairBinding,
};
use serde_json::Value;

use crate::application::contracts::{
    CreationCrewAssignment, CrewCandidate, CrewPoolDraw, InspectionPool, RetryKey,
};

/// The crew pools an admitting pipeline captured, as task code names them.
pub(in crate::application::task) type CrewPools = <crate::OrbitRuntime as TaskJobPort>::CrewPools;

/// The job-dispatch and crew-pool operations task code calls, implemented
/// by `job`.
pub(in crate::application) trait TaskJobPort {
    /// The crew pools an admitting pipeline captured. Task code passes them
    /// through without reading them; `job` owns their shape.
    type CrewPools;

    /// One admission pass's view of whether the PR route would find a forge;
    /// it reads the checkout's remotes at most once.
    type PrForge: Default;

    /// The crew a task is created or re-rated with: `explicit` verbatim,
    /// else the purpose pool its tags name, then its complexity pool, then
    /// the default crew; `None` when the workspace names no crew. `random`
    /// supplies the draw's tickets.
    fn creation_crew(
        &self,
        complexity: Option<TaskComplexity>,
        explicit: Option<&str>,
        tags: &[String],
        requires_labels: &[String],
        context_files: &[String],
        random: &mut dyn FnMut() -> Result<u64, OrbitError>,
    ) -> Result<Option<CreationCrewAssignment>, OrbitError>;

    /// A fresh ticket from the system RNG for a crew draw.
    fn random_crew_draw_ticket(&self) -> Result<u64, OrbitError>;

    /// Reproducible tickets for a crew draw that must come out the same
    /// when it is retried, seeded by `seed`.
    fn seeded_crew_draw_tickets(
        &self,
        seed: String,
    ) -> Box<dyn FnMut() -> Result<u64, OrbitError> + 'static>;

    /// The enabled members of `complexity`'s captured pool, with the pool's
    /// provenance; `None` when the pool is unset or empty.
    fn complexity_pool_members(
        &self,
        complexity: Option<TaskComplexity>,
        pools: &Self::CrewPools,
    ) -> Result<Option<(Vec<CrewCandidate>, String)>, OrbitError>;

    /// `task`'s candidates before any provider failure hold: its validated
    /// pool assignment or explicit pin, else its complexity pool, else the
    /// default chain, with their provenance.
    fn crew_candidates_before_holds(
        &self,
        task: &Task,
        pools: &Self::CrewPools,
    ) -> Result<(Vec<CrewCandidate>, String), OrbitError>;

    /// The provenance `task`'s crew was assigned with: its recorded
    /// `crew_source`, else the latest assignment history entry naming its
    /// current crew; `None` for a task with no crew or no such evidence.
    fn recorded_crew_source(&self, task: &Task) -> Result<Option<String>, OrbitError>;

    /// Whether `task`, last delivered by an owner PR run, has a pull request
    /// or a publication-preservation record that no failure-handoff receipt
    /// binds, so the next admission would refuse it.
    fn unbound_publication(&self, task: &Task) -> Result<bool, OrbitError>;

    /// The validation gates this process holds a candidate to: a claimed
    /// leaf's captured owner gates, else this host's configuration.
    fn candidate_validation_gates(&self) -> HostValidationGates;

    /// Whether a task with `tags` would ship through the PR pipeline in
    /// `mode` while `forge` finds no forge remote for this checkout.
    fn pr_route_lacks_forge(&self, forge: &Self::PrForge, tags: &[String], mode: ShipMode) -> bool;

    /// Submit the catalog routine `job_name` with `input`, stamped with this
    /// workspace's `.orbit` directory so its worker verifies where it runs.
    /// A `retry_key` resolves an earlier run of the same key instead of
    /// admitting another. Returns the run ID.
    fn submit_routine_run(
        &self,
        job_name: &str,
        input: Value,
        actor: &str,
        retry_key: Option<RetryKey<'_>>,
        trigger: JobRunTrigger,
    ) -> Result<String, OrbitError>;

    /// The run `run_id` as recorded, without stale-run reconciliation.
    fn observed_job_run(&self, run_id: &str) -> Result<JobRun, OrbitError>;

    /// Whether `run` is running under a recorded owner process whose
    /// identity is verified to still be executing it.
    fn run_owner_verified(&self, run: &JobRun) -> bool;

    /// Whether `run`'s recorded owner process is conclusively gone, asked
    /// independently of the run's persisted state.
    fn run_owner_stopped(&self, run: &JobRun) -> bool;

    /// The run `run_id` as stored, without reconciliation or liveness checks.
    fn stored_job_run(&self, run_id: &str) -> Result<Option<JobRun>, OrbitError>;

    /// The persisted pipeline state of `run_id`, as stored.
    fn stored_run_state(&self, run_id: &str) -> Result<Option<PipelineState>, OrbitError>;

    /// The preservation record for a published owner run of `task`, when it
    /// published through its own pipeline.
    fn published_owner_preservation(
        &self,
        task: &Task,
        run_id: &str,
    ) -> Result<Option<TaskHistoryEntry>, OrbitError>;

    /// The authenticated failure-handoff preservation record for `task` and
    /// the binding its next run resumes.
    fn failure_handoff_preservation(
        &self,
        task: &Task,
    ) -> Result<(TaskHistoryEntry, SourceRepairBinding), OrbitError>;

    /// The authenticated preservation record for the pull request `task`'s
    /// escalated run published through its own pipeline steps and its
    /// failure handoff kept, and the binding its next run resumes.
    fn escalated_publication_preservation(
        &self,
        task: &Task,
    ) -> Result<(TaskHistoryEntry, SourceRepairBinding), OrbitError>;

    /// The preservation record to append for `entry`, or `None` when an
    /// identical record is already on `task`.
    fn preservation_append(
        &self,
        task: &Task,
        entry: TaskHistoryEntry,
    ) -> Result<Option<TaskHistoryEntry>, OrbitError>;

    /// The default crew for an inspection-requiring activity at `tier` when
    /// no pool from that tier upward can inspect.
    fn inspection_default_crew(&self, tier: TaskComplexity) -> Result<Crew, OrbitError>;

    /// The first pool from `tier` upward whose enabled members can inspect.
    fn inspection_pool(&self, tier: TaskComplexity) -> Result<Option<InspectionPool>, OrbitError>;

    /// Redraw `task`'s crew for a new `complexity`, returning the history
    /// entry to commit with the redrawn crew.
    fn rerated_crew(
        &self,
        task: &mut Task,
        complexity: Option<TaskComplexity>,
    ) -> Result<Option<TaskHistoryEntry>, OrbitError>;

    /// The crew pools captured for `input`'s run, or freshly captured pools.
    fn crew_pools_for_input(&self, input: &Value) -> Result<Self::CrewPools, OrbitError>;

    /// Draw one crew from `candidates` under `source` with `random` tickets.
    fn crew_pool_draw(
        &self,
        candidates: Vec<CrewCandidate>,
        source: &str,
        random: &mut dyn FnMut() -> Result<u64, OrbitError>,
    ) -> Result<CrewPoolDraw, OrbitError>;

    /// The enabled members of the purpose pool `task` was assigned from.
    fn purpose_candidates(
        &self,
        task: &Task,
    ) -> Result<Option<(Vec<CrewCandidate>, String)>, OrbitError>;

    /// What a delivery run committed and landed for `task_id`.
    fn delivery_observation(
        &self,
        task_id: &str,
        run_id: Option<&str>,
    ) -> Result<RunDeliveryObservation, OrbitError>;
}
