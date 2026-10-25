macro_rules! review_methods {
    () => {
        // ── Step-recovery hooks [ORB-11332] ────────────────────────────────

        /// Ask before dispatching a step-recovery hook. A host that gates
        /// recovery answers `Denied` with the reason.
        fn authorize_step_recovery(
            &self,
            run_id: &str,
            step_id: &str,
        ) -> Result<StepRecoveryAdmission, OrbitError>;
        /// Allocate the run-local file one `step_failure_recovery` invocation
        /// writes its decision to, beneath the assigned worktree's scratch
        /// directory [ORB-14152]. Hosts without the capability return `Ok(None)`:
        /// the invocation runs without a slot and keeps the legacy admission, one
        /// post-recovery attempt whenever recovery completes. An error refuses
        /// the recovery before its provider launches.
        fn allocate_step_recovery_decision(
            &self,
            request: &StepRecoveryDecisionRequest,
        ) -> Result<Option<StepRecoveryDecisionSlot>, OrbitError>;
        /// Read back and verify the decision a completed invocation left in
        /// `slot`. An error means the slot could not be read and authorizes no
        /// post-recovery attempt.
        fn read_step_recovery_decision(
            &self,
            slot: &StepRecoveryDecisionSlot,
        ) -> Result<StepRecoveryDecisionRead, OrbitError>;
        /// Revalidate the live owner immediately before a recovery hook asks the
        /// host process to mutate Git metadata. Agent subprocesses cannot confer
        /// this authority through their response payload.
        fn validate_step_recovery_mutation(
            &self,
            run_id: &str,
            step_id: &str,
            task_ids: &[String],
            workspace_path: &Path,
        ) -> Result<(), OrbitError>;
        /// The immutable source-repair admission on this run, if any. Hosts that
        /// cannot admit this recovery return none without requiring run storage.
        fn source_repair_admission(
            &self,
            run_id: &str,
        ) -> Result<Option<orbit_types::workflow::SourceRepairAdmission>, OrbitError>;
        /// Authenticate an owner-local publication preserved by final recovery.
        fn owner_published_candidate(
            &self,
            task_id: &str,
            source_run_id: Option<&str>,
        ) -> Result<Option<orbit_types::workflow::SourceRepairBinding>, OrbitError>;
        /// Read publication authority from this run's successful host adoption.
        fn owner_publication_binding(
            &self,
            run_id: &str,
        ) -> Result<Option<orbit_types::workflow::SourceRepairBinding>, OrbitError>;
        /// Recheck the durable operator admission and exact source/scope before
        /// applying a cancelled owner's published candidate. Other hosts refuse.
        fn validate_source_repair(&self, run_id: &str) -> Result<(), OrbitError>;
        /// Admit one `--auto-fix` repair of a pull request's failed required
        /// checks or merge conflict before any agent runs. The host draws the
        /// run's repair crew from `workflow.purpose_crews.auto_fix` (the low
        /// complexity pool while that purpose is unset) and decides the
        /// request against the pull request's durable ledger under the primary
        /// task's lock, recording an admitted attempt in the same critical
        /// section.
        fn admit_pr_auto_fix(
            &self,
            request: &PrAutoFixRequest,
        ) -> Result<PrAutoFixAdmission, OrbitError>;

        /// Reserve the cumulative infrastructure rerun before mutation, under the
        /// owner task lock. Hosts without a durable ledger refuse by default.
        fn admit_pr_infrastructure(
            &self,
            _execution: &str,
            _task_ids: &[String],
            _evidence: &orbit_types::workflow::PrInfrastructureEvidence,
        ) -> Result<orbit_types::workflow::PrInfrastructureAdmission, OrbitError> {
            Ok(orbit_types::workflow::PrInfrastructureAdmission::Refused(
                "this host cannot durably reserve infrastructure recovery".into(),
            ))
        }

        /// Recheck current accepted handoff authority/native evidence and reserve
        /// its base update under the same owner lock. Generic hosts refuse.
        fn admit_handoff_base_update(
            &self,
            _handoff_id: &str,
            _request: &orbit_types::workflow::PrBaseUpdateReservation,
        ) -> Result<orbit_types::workflow::PrBaseUpdateAdmission, OrbitError> {
            Ok(orbit_types::workflow::PrBaseUpdateAdmission::Refused(
                "current handoff authority is unavailable".into(),
            ))
        }

        /// Reserve the one cumulative provider base update under the owning task
        /// lock. A host without durable owner admission cannot mutate.
        fn admit_pr_base_update(
            &self,
            _task_ids: &[String],
            _request: &orbit_types::workflow::PrBaseUpdateReservation,
        ) -> Result<orbit_types::workflow::PrBaseUpdateAdmission, OrbitError> {
            Ok(orbit_types::workflow::PrBaseUpdateAdmission::Refused(
                "this host cannot durably reserve a base update".into(),
            ))
        }

        // ── Before-PR review coverage [ORB-11333] ───────────────────────────

        /// Record how a reviewed candidate actually landed after a managed
        /// merge. Hosts without review evidence keep the pre-existing behavior.
        fn record_review_landing(&self, request: &ReviewLandingRequest) -> Result<(), OrbitError>;

        /// Close a review attempt that ended without a verdict, charging the
        /// reviewer runtime spent, so a failed or terminated reviewer step never
        /// leaves its attempt open. Hosts without review evidence have nothing
        /// to close.
        fn release_review_attempt(&self, request: &ReviewReleaseRequest) -> Result<(), OrbitError>;

        /// Record a reviewer invocation starting or ending for its attempt.
        /// For a start, returns the seconds the invocation may run, which become
        /// the reviewer process's wall clock whatever the activity declares
        /// [ORB-13992]. Hosts without review evidence have nothing to charge or
        /// bound, and the activity's own wall clock applies.
        fn record_reviewer_invocation(
            &self,
            request: &ReviewerInvocationRequest,
        ) -> Result<Option<u64>, OrbitError>;

        /// The defect settlement would find in the report of a reviewer that
        /// just returned, when the reviewer can correct it without rerunning a
        /// check [ORB-14616]. The engine then asks that reviewer, once, to
        /// correct the report before settlement. Hosts without review evidence
        /// have nothing to judge.
        fn review_report_correction(
            &self,
            request: &ReviewReportCorrectionRequest,
        ) -> Result<Option<String>, OrbitError>;

        // ── Owner landing consumer [ORB-12499] ─────────────────────────────

        /// Read the owner's authorized handoff for a landing attempt. Hosts without
        /// an owner coordination store have no landing work.
        fn handoff_landing_context(
            &self,
            handoff_id: &str,
        ) -> Result<HandoffLandingContext, OrbitError>;

        /// Record a landing step durably. The host rechecks current authority, the
        /// exact candidate and the pinned validation evidence inside its own
        /// transaction; an error refuses the step.
        fn record_handoff_landing(&self, update: &HandoffLandingUpdate) -> Result<(), OrbitError>;

        // ── Claimed distributed leaf execution [ORB-12616] ─────────────────

        /// The machine this runtime executes on, as the
        /// [`orbit_types::task::ExecutionLocation`] of its own runs names it. Run
        /// ids are unique only per machine [ORB-13649], so a task bound to a run
        /// on any other machine — or on any machine, when this host has no
        /// identity — names a run this store does not hold.
        fn local_machine_id(&self) -> Option<String>;

        /// [ORB-14603] The candidate the task's latest claim settlement kept, as
        /// offered to this machine's own run of the task, or why that run
        /// implements afresh. Hosts that keep no claims kept none.
        fn kept_claim_candidate(
            &self,
            task_id: &str,
        ) -> Result<Option<KeptClaimCandidate>, OrbitError>;

        /// The trusted claim this process is executing under. Hosts with no worker
        /// binding have no claimed execution and refuse: a claimed leaf activity
        /// must never fall back to an unauthenticated local identity.
        fn claim_execution_context(&self) -> Result<ClaimExecutionContext, OrbitError>;

        /// Attach captured validation or required task evidence to the claimed
        /// task on the owner, using the path's media type. The evidence the owner
        /// later re-reads lives in its coordination store, not on the executor's disk.
        fn attach_claim_validation_log(
            &self,
            path: &str,
            content: Vec<u8>,
        ) -> Result<(), OrbitError>;

        /// Record the typed handoff as this claim's durable pending settlement.
        /// It commits locally before any owner call, so a disconnect leaves
        /// exactly one immutable settlement for an idempotent retry.
        fn record_claim_handoff(
            &self,
            handoff: &orbit_types::workflow::handoff::TaskHandoff,
        ) -> Result<(), OrbitError>;

        /// Attach one captured required-validation log to a task the calling run
        /// owns, as owner-side evidence of the candidate it validated
        /// [ORB-13915]. The host refuses a task this run does not own.
        fn attach_task_validation_log(
            &self,
            task_id: &str,
            run_id: &str,
            path: &str,
            content: Vec<u8>,
        ) -> Result<(), OrbitError>;

        /// Record a passing isolated candidate replay in task history and file
        /// each flaky signature once in the owner's workspace.
        fn record_flaky_validation(
            &self,
            task_id: &str,
            record: &orbit_types::workflow::FlakyValidation,
        ) -> Result<(), OrbitError>;
    };
}
pub(super) use review_methods;
