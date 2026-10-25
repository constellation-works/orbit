macro_rules! recovery_methods {
    () => {
        /// Resolve a deny-mode activity's callable tools for one agent launch.
        ///
        /// `effective_tools` is every registered agent-facing tool the disallow
        /// list does not cover. A requirement never overrides that list.
        /// `agent_implement`, and any activity that is not a shipped
        /// non-implementer, admits `required_tools` fail-closed: unknown,
        /// inactive, malformed and denied requirements refuse dispatch, naming
        /// the tool and `activity`. Final recovery, step recovery, reviewers and
        /// the other shipped non-implementers drop a covered requirement from
        /// `requested_tools` and record why on `omitted_requirement_notes`.
        /// A host without a tool registry cannot compute the set, so it refuses
        /// rather than guessing.
        fn resolve_activity_tool_denials(
            &self,
            task_ids: &[String],
            activity: &str,
            disallow_list: &[String],
        ) -> Result<ResolvedActivityTools, DispatchError>;

        /// Compose and validate the exact task-scoped tools for one agent launch.
        /// Hosts without a task/tool registry preserve the activity baseline.
        fn resolve_activity_tools(
            &self,
            task_ids: &[String],
            baseline_tools: &[String],
        ) -> Result<ResolvedActivityTools, DispatchError>;

        /// Persist a durable checkpoint after a completed top-level job step
        /// (ORB-10002). `output` is the completing step's own raw output;
        /// `compound_outputs` holds the other pipeline entries a compound step
        /// (`parallel:`, `fan_out:`, `loop:`) exposed — nested step outputs and
        /// nested fan-in aliases — and is empty for a target step. The host
        /// accumulates by step, so the bytes handed over per checkpoint never
        /// grow with the run.
        ///
        /// Hosts with run persistence (orbit-core) record this into the run's
        /// `PipelineState` (`step_outputs[step_index]`,
        /// `compound_outputs[step_index]`, and `pipeline` by key) so an
        /// interrupted run can be resumed without re-executing completed steps.
        /// Both halves belong to one write: a step recorded as completed without
        /// its compound outputs would resume with those entries missing. A host
        /// without run storage records nothing. Checkpoint failures are non-fatal to the run: the executor
        /// logs and continues.
        fn checkpoint_step(
            &self,
            run_id: &str,
            step_index: u32,
            step_id: &str,
            output: &Value,
            compound_outputs: &BTreeMap<String, Value>,
        ) -> Result<(), DispatchError>;

        /// Persist diagnostic output from an incomplete step without marking it
        /// successful or making it eligible for resume skipping.
        fn checkpoint_step_diagnostic(
            &self,
            _run_id: &str,
            _step_id: &str,
            _output: &Value,
        ) -> Result<(), DispatchError> {
            Ok(())
        }

        /// Persist a successful terminal failure activity separately from normal
        /// step checkpoints. The failed step remains failed; this records only the
        /// recovery evidence a later resume may authenticate.
        fn checkpoint_failure_activity(
            &self,
            run_id: &str,
            activity_name: &str,
            failed_step_id: &str,
            output: &Value,
        ) -> Result<(), DispatchError>;

        /// Read only this run's bounded, redacted worker log tail for built-in
        /// step and final recovery. `None` means the run has no worker log; read failures must
        /// remain distinguishable from an empty log. This host capability is
        /// internal and grants no agent-facing run observation tool.
        fn final_recovery_log_tail(&self, run_id: &str) -> Result<Option<String>, OrbitError>;

        /// [ORB-13907] Admit a job's final recovery for a failed run, once.
        ///
        /// A host with run storage records the admission in the run's state
        /// before answering `Admitted`, so neither a crash during the activity
        /// nor any resume of the run invokes it again; it skips when final
        /// recovery is disabled (an empty `workflow.purpose_crews.final_recovery`), when
        /// the run already spent it, or when the run is no longer running. A host
        /// without run storage skips: it cannot promise "once".
        fn admit_final_recovery(
            &self,
            run_id: &str,
            request: &FinalRecoveryAdmissionRequest,
        ) -> Result<FinalRecoveryAdmission, OrbitError>;

        /// Keep one absolute completion budget across retries of a persisted run.
        /// An unpersisted host uses the current invocation's deadline.
        fn completion_deadline(
            &self,
            _run_id: &str,
            _pull_request: &str,
            proposed: u64,
        ) -> Result<u64, OrbitError> {
            Ok(proposed)
        }

        /// Bind native validation to task meaning and the effective completion policy.
        /// Hosts without this evidence never reuse a published completion prefix.
        fn completion_policy_digest(
            &self,
            _run_id: &str,
            _task_ids: &[String],
        ) -> Result<Option<String>, OrbitError> {
            Ok(None)
        }

        /// Classify a transient completion observation after authenticating the
        /// publication and its receipts. This grants no authority to retry mutations.
        fn completion_observation_admission(
            &self,
            run_id: &str,
            task_id: &str,
            step_id: &str,
        ) -> Result<CompletionObservationAdmission, OrbitError>;

        /// Authenticate an unchanged publication before the once-only final recovery
        /// retries a read-only completion observation in the same active execution.
        fn authenticate_completion_tail(
            &self,
            _run_id: &str,
            _task_id: &str,
            _step_id: &str,
        ) -> Result<bool, OrbitError> {
            Ok(false)
        }

        /// [ORB-13907] Act on an admitted final recovery's decision: record it,
        /// and apply every decision but `resume` to the task through the
        /// deterministic applier — or, for a claimed leaf, hand it to the claim
        /// settlement instead of writing the owner's task.
        fn apply_final_recovery(
            &self,
            run_id: &str,
            application: &FinalRecoveryApplication,
        ) -> Result<FinalRecoveryApplied, OrbitError>;

        /// Reserve the host-only identity of one preparation or rebase recovery
        /// of `step_id`, before issuing its certificate (and, for conflicts,
        /// before its provider runs).
        ///
        /// The host assigns the attempt; the caller only carries it, in memory, to
        /// the checkpoint it stamps as `recovery_attempt`. A later reservation for
        /// the same run and step supersedes this one, so only the newest admitted
        /// recovery can be certified. Hosts without a recovery authority refuse,
        /// which stops the recovery before the provider launches.
        fn begin_rebase_recovery_attempt(
            &self,
            run_id: &str,
            step_id: &str,
            scope: &RebaseRecoveryAttemptScope,
        ) -> Result<u64, DispatchError>;

        /// Persist an exact host-validated preparation or recovered rebase before
        /// reporting success. Preparations use separate step identities from
        /// rewrites and bind their entire observation payload. Unlike ordinary
        /// step checkpoints, durability failure is fatal.
        /// `output` carries the attempt reserved by
        /// [`Self::begin_rebase_recovery_attempt`] as `recovery_attempt`.
        fn checkpoint_rebase_recovery(
            &self,
            run_id: &str,
            step_id: &str,
            output: &Value,
        ) -> Result<(), DispatchError>;

        /// Whether `checkpoint` is exactly the current recovery evidence this host
        /// certified for `run_id` / `step_id`: its own attempt's evidence, with no
        /// later attempt of that step certified since.
        ///
        /// The run store a checkpoint is read back from is writable by managed
        /// leaves, so the stored bytes are progress data. Authority lives in a
        /// host-only record this method consults. Hosts without one certify
        /// nothing and therefore authenticate nothing.
        fn verify_rebase_recovery(
            &self,
            run_id: &str,
            step_id: &str,
            checkpoint: &Value,
        ) -> Result<bool, OrbitError>;

        /// Every recovery attempt this host reserved for `run_id` / `step_id`,
        /// oldest first, read from the same host-only record that assigns them.
        /// Completion counts the distinct bases a step rebased onto to bound how
        /// often it follows a moving base [ORB-14393].
        fn rebase_recovery_attempts(
            &self,
            run_id: &str,
            step_id: &str,
        ) -> Result<Vec<RebaseRecoveryAttemptScope>, OrbitError>;

        fn tool_context_for_activity(
            &self,
            run_id: Option<&str>,
            fs_profile: Option<&str>,
            fs_audit: Option<Arc<dyn FsAuditLogger>>,
            proc_allowed_programs: Option<&[String]>,
        ) -> ToolContext;

        /// Record one dispatch's invocation: its provider, model, crew account
        /// and trace.
        fn persist_invocation_trace(
            &self,
            job_run_id: &str,
            activity_id: &str,
            input: &Value,
            invocation: &DispatchInvocationTrace,
        ) -> Result<(), DispatchError>;

        /// Resolve the crew selected for an activity dispatch. The engine passes
        /// rendered activity input when it contains an explicit `crew`; otherwise
        /// it passes the run input so the run's resolved crew is the fallback.
        /// Hosts without a crew registry may return `None` only when no explicit
        /// crew was requested, preserving inline settings in isolated tests.
        fn agent_crew_config_for_input(
            &self,
            input: &Value,
        ) -> Result<Option<CrewConfig>, DispatchError>;
    };
}
pub(super) use recovery_methods;
