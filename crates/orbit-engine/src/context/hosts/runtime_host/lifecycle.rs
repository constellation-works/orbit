macro_rules! lifecycle_methods {
    () => {
        /// Start this run's plugin broker before a sandboxed provider is spawned.
        ///
        /// `Ok(None)` means the host offers no broker. An error names why a
        /// broker-capable host could not bind one; the step still runs without
        /// it.
        fn start_plugin_broker(
            &self,
            run: &PluginBrokerRun,
        ) -> Result<Option<Box<dyn PluginBrokerHandle>>, OrbitError>;

        fn register_worker_pid_namespace(&self, pid: u32) -> Result<(), OrbitError>;

        fn register_worker_process(&self, pid: u32) -> Result<(), OrbitError>;

        fn worker_invocation(&self) -> Option<orbit_types::tool::WorkerInvocation>;
        /// Record a direct landing's intent before it lands. A host without a
        /// scheduler store observes nothing.
        fn record_direct_landing_intent(
            &self,
            request: &orbit_types::workflow::automation::DirectLandingRequest,
        ) -> Result<(), OrbitError>;
        /// Coverage evidence `task_id` submitted as a delivery automation action,
        /// when the automation's own settlement accepts or would accept it for the
        /// frozen batch [ORB-14837]. A host without delivery automation has none.
        fn accepted_automation_coverage(
            &self,
            task_id: &str,
        ) -> Result<Option<orbit_types::workflow::automation::CoverageEvidence>, OrbitError>;

        fn insert_job_run(
            &self,
            job_id: &str,
            attempt: u32,
            scheduled_at: chrono::DateTime<chrono::Utc>,
            input: Option<serde_json::Value>,
            retry_source_run_id: Option<String>,
        ) -> Result<JobRun, OrbitError>;
        fn mark_job_run_running(
            &self,
            run_id: &str,
            started_at: chrono::DateTime<chrono::Utc>,
            pid: u32,
        ) -> Result<JobRunStartOutcome, OrbitError>;
        fn complete_job_run_step(
            &self,
            run_id: &str,
            params: &JobRunStepParams,
        ) -> Result<bool, OrbitError>;
        fn finalize_job_run(
            &self,
            run_id: &str,
            state: JobRunState,
            finished_at: chrono::DateTime<chrono::Utc>,
            duration_ms: Option<u64>,
        ) -> Result<bool, OrbitError>;
        fn get_job_run(&self, run_id: &str) -> Result<Option<JobRun>, OrbitError>;
        fn read_run_state(&self, run_id: &str) -> Result<Option<PipelineState>, OrbitError>;
        fn write_run_state(&self, run_id: &str, state: &PipelineState) -> Result<(), OrbitError>;

        fn get_task(&self, task_id: &str) -> Result<Task, OrbitError>;
        /// Retrieve only one named, complete inline evidence payload. Implementations
        /// enforce the existing artifact read bound and owning task/path authority.
        fn get_task_artifact(
            &self,
            task_id: &str,
            path: &str,
        ) -> Result<Option<TaskArtifact>, OrbitError>;
        fn get_task_artifacts(&self, task_id: &str) -> Result<Vec<TaskArtifact>, OrbitError>;
        fn get_task_comments(&self, task_id: &str) -> Result<Vec<TaskComment>, OrbitError>;
        fn get_task_history(&self, task_id: &str) -> Result<Vec<TaskHistoryEntry>, OrbitError>;
        fn list_tasks_filtered(
            &self,
            status: Option<TaskStatus>,
            priority: Option<TaskPriority>,
            parent_id: Option<&str>,
            job_run_id: Option<&str>,
            external_ref: Option<&ExternalRef>,
            has_external_ref_system: Option<&str>,
        ) -> Result<Vec<Task>, OrbitError>;
        /// The tasks run `run_id` bound on the machine that executes it.
        ///
        /// A run id is unique only within one machine's store, so two machines'
        /// runs can share one [ORB-13649]. A runtime that records where each
        /// binding executed scopes this lookup to the executing machine; one
        /// that does not reads by run id alone.
        fn list_run_tasks(&self, run_id: &str) -> Result<Vec<Task>, OrbitError>;

        fn start_task(
            &self,
            task_id: &str,
            note: Option<String>,
            comment: Option<String>,
        ) -> Result<Task, OrbitError>;
        fn admit_task_for_workflow(
            &self,
            task_id: &str,
            workflow: &str,
        ) -> Result<Task, OrbitError>;
        fn update_task_from_activity(
            &self,
            task_id: &str,
            update: TaskActivityUpdate,
        ) -> Result<Task, OrbitError>;
        fn apply_task_automation_update(
            &self,
            task_id: &str,
            update: TaskAutomationUpdate,
        ) -> Result<(), OrbitError>;

        /// Append an exact `file:` selector to `task_id` for each of `paths` its
        /// selectors do not cover, recording `step` and `activity` as the
        /// provenance in task history, and return the selectors appended.
        ///
        /// Agents may change any path the work requires; delivery widens the
        /// task's declaration rather than refusing the change. A claimed leaf
        /// widens nothing here: the owner widens at handoff acceptance. Hosts
        /// without task records widen nothing.
        fn widen_task_context_files(
            &self,
            task_id: &str,
            run_id: &str,
            step: ContextWideningStep,
            activity: &str,
            paths: &[String],
        ) -> Result<Vec<String>, OrbitError>;
    };
}
pub(super) use lifecycle_methods;
