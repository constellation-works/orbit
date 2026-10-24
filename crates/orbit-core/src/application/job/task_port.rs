//! Job dispatch's side of the task-owned job port [ORB-17936].

use orbit_common::OrbitError;
use orbit_types::task::{Task, TaskComplexity};
use orbit_types::workflow::{HostValidationGates, JobRun, JobRunTrigger, ShipMode};
use serde_json::{Value, json};

use super::crew_pools::{CapturedCrewPools, random_crew_ticket, seeded_crew_ticket};
use super::delivery::{PrForgeCheck, tags_route_through_pr_pipeline};
use super::pipeline::{PipelineSubmission, ROUTINE_DISPATCH_ORBIT_DIR_FIELD};
use super::{RunOwnerLiveness, run_owner_liveness, running_run_has_verified_owner};
use crate::OrbitRuntime;
use crate::application::contracts::{CreationCrewAssignment, CrewCandidate, RetryKey};
use crate::application::task::TaskJobPort;

impl TaskJobPort for OrbitRuntime {
    type CrewPools = CapturedCrewPools;
    type PrForge = PrForgeCheck;

    fn creation_crew(
        &self,
        complexity: Option<TaskComplexity>,
        explicit: Option<&str>,
        tags: &[String],
        context_files: &[String],
        mut random: &mut dyn FnMut() -> Result<u64, OrbitError>,
    ) -> Result<Option<CreationCrewAssignment>, OrbitError> {
        self.creation_crew_assignment(complexity, explicit, tags, context_files, &mut random)
    }

    fn random_crew_draw_ticket(&self) -> Result<u64, OrbitError> {
        random_crew_ticket()
    }

    fn seeded_crew_draw_tickets(
        &self,
        seed: String,
    ) -> Box<dyn FnMut() -> Result<u64, OrbitError> + 'static> {
        Box::new(seeded_crew_ticket(seed))
    }

    fn complexity_pool_members(
        &self,
        complexity: Option<TaskComplexity>,
        pools: &CapturedCrewPools,
    ) -> Result<Option<(Vec<CrewCandidate>, String)>, OrbitError> {
        self.complexity_pool_candidates(complexity, pools)
    }

    fn crew_candidates_before_holds(
        &self,
        task: &Task,
        pools: &CapturedCrewPools,
    ) -> Result<(Vec<CrewCandidate>, String), OrbitError> {
        self.unheld_task_crew_candidates(task, pools)
    }

    fn recorded_crew_source(&self, task: &Task) -> Result<Option<String>, OrbitError> {
        self.task_crew_source(task)
    }

    fn unbound_publication(&self, task: &Task) -> Result<bool, OrbitError> {
        self.unbound_publication_on_record(task)
    }

    fn candidate_validation_gates(&self) -> HostValidationGates {
        self.host_validation_gates()
    }

    fn pr_route_lacks_forge(&self, forge: &PrForgeCheck, tags: &[String], mode: ShipMode) -> bool {
        tags_route_through_pr_pipeline(tags, mode) && forge.require(self).is_err()
    }

    fn submit_routine_run(
        &self,
        job_name: &str,
        mut input: Value,
        actor: &str,
        retry_key: Option<RetryKey<'_>>,
        trigger: JobRunTrigger,
    ) -> Result<String, OrbitError> {
        input[ROUTINE_DISPATCH_ORBIT_DIR_FIELD] = json!(self.shared_root().to_string_lossy());
        let submission = PipelineSubmission {
            retry_key,
            trigger,
            ..PipelineSubmission::catalog(job_name, input, Some(actor))
        };
        self.submit_keyed_pipeline_run(submission)
            .map(|(result, _)| result.run_id)
    }

    fn observed_job_run(&self, run_id: &str) -> Result<JobRun, OrbitError> {
        self.show_job_run_observed(run_id)
    }

    fn run_owner_verified(&self, run: &JobRun) -> bool {
        running_run_has_verified_owner(run)
    }

    fn run_owner_stopped(&self, run: &JobRun) -> bool {
        run_owner_liveness(run) == RunOwnerLiveness::Stopped
    }
}
