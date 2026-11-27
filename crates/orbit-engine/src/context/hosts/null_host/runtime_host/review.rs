//! The [`ReviewGate`] answers of every [`NullRuntimeHost`].

use orbit_common::OrbitError;
use orbit_store::contract::KeptClaimCandidate;
use std::path::Path;

use crate::context::hosts::{
    ClaimExecutionContext, HandoffLandingContext, HandoffLandingUpdate, PrAutoFixAdmission,
    PrAutoFixRequest, ReviewLandingRequest, ReviewReleaseRequest, ReviewReportCorrectionRequest,
    ReviewerInvocationRequest, StepRecoveryAdmission, StepRecoveryDecisionRead,
    StepRecoveryDecisionRequest, StepRecoveryDecisionSlot,
};

use crate::context::hosts::{NullRuntimeHost, ReviewGate};

impl<T: NullRuntimeHost> ReviewGate for T {
    fn authorize_step_recovery(
        &self,
        run_id: &str,
        step_id: &str,
    ) -> Result<StepRecoveryAdmission, OrbitError> {
        NullRuntimeHost::authorize_step_recovery(self, run_id, step_id)
    }

    fn allocate_step_recovery_decision(
        &self,
        request: &StepRecoveryDecisionRequest,
    ) -> Result<Option<StepRecoveryDecisionSlot>, OrbitError> {
        NullRuntimeHost::allocate_step_recovery_decision(self, request)
    }

    fn read_step_recovery_decision(
        &self,
        slot: &StepRecoveryDecisionSlot,
    ) -> Result<StepRecoveryDecisionRead, OrbitError> {
        NullRuntimeHost::read_step_recovery_decision(self, slot)
    }

    fn source_repair_admission(
        &self,
        run_id: &str,
    ) -> Result<Option<orbit_types::workflow::SourceRepairAdmission>, OrbitError> {
        NullRuntimeHost::source_repair_admission(self, run_id)
    }

    fn owner_published_candidate(
        &self,
        task_id: &str,
        source_run_id: Option<&str>,
    ) -> Result<Option<orbit_types::workflow::SourceRepairBinding>, OrbitError> {
        NullRuntimeHost::owner_published_candidate(self, task_id, source_run_id)
    }

    fn owner_publication_binding(
        &self,
        run_id: &str,
    ) -> Result<Option<orbit_types::workflow::SourceRepairBinding>, OrbitError> {
        NullRuntimeHost::owner_publication_binding(self, run_id)
    }

    fn validate_source_repair(&self, run_id: &str) -> Result<(), OrbitError> {
        NullRuntimeHost::validate_source_repair(self, run_id)
    }

    fn admit_pr_auto_fix(
        &self,
        request: &PrAutoFixRequest,
    ) -> Result<PrAutoFixAdmission, OrbitError> {
        NullRuntimeHost::admit_pr_auto_fix(self, request)
    }

    fn admit_handoff_base_update(
        &self,
        handoff_id: &str,
        request: &orbit_types::workflow::PrBaseUpdateReservation,
    ) -> Result<orbit_types::workflow::PrBaseUpdateAdmission, OrbitError> {
        NullRuntimeHost::admit_handoff_base_update(self, handoff_id, request)
    }

    fn admit_pr_base_update(
        &self,
        task_ids: &[String],
        request: &orbit_types::workflow::PrBaseUpdateReservation,
    ) -> Result<orbit_types::workflow::PrBaseUpdateAdmission, OrbitError> {
        NullRuntimeHost::admit_pr_base_update(self, task_ids, request)
    }

    fn refresh_delivery_pull_request(
        &self,
        request: &orbit_types::workflow::DeliveryRefreshRequest,
    ) -> Result<orbit_types::workflow::PullRequestRefreshOutcome, OrbitError> {
        NullRuntimeHost::refresh_delivery_pull_request(self, request)
    }

    fn audited_delivery_refreshes(
        &self,
        task_id: &str,
        pr_number: &str,
        head_branch: &str,
        base_branch: &str,
        published: &str,
        reported: &str,
    ) -> Result<Vec<String>, OrbitError> {
        NullRuntimeHost::audited_delivery_refreshes(
            self,
            task_id,
            pr_number,
            head_branch,
            base_branch,
            published,
            reported,
        )
    }

    fn admit_ci_rerun(
        &self,
        task_ids: &[String],
        handoff_id: Option<&str>,
        plan: &orbit_types::workflow::CiRerunPlan,
        now_ms: u64,
    ) -> Result<orbit_types::workflow::CiRerunAdmission, OrbitError> {
        NullRuntimeHost::admit_ci_rerun(self, task_ids, handoff_id, plan, now_ms)
    }

    fn admit_pr_infrastructure(
        &self,
        execution: &str,
        task_ids: &[String],
        evidence: &orbit_types::workflow::PrInfrastructureEvidence,
    ) -> Result<orbit_types::workflow::PrInfrastructureAdmission, OrbitError> {
        NullRuntimeHost::admit_pr_infrastructure(self, execution, task_ids, evidence)
    }

    fn validate_step_recovery_mutation(
        &self,
        run_id: &str,
        step_id: &str,
        task_ids: &[String],
        workspace_path: &Path,
    ) -> Result<(), OrbitError> {
        NullRuntimeHost::validate_step_recovery_mutation(
            self,
            run_id,
            step_id,
            task_ids,
            workspace_path,
        )
    }

    fn record_review_landing(&self, request: &ReviewLandingRequest) -> Result<(), OrbitError> {
        NullRuntimeHost::record_review_landing(self, request)
    }

    fn release_review_attempt(&self, request: &ReviewReleaseRequest) -> Result<(), OrbitError> {
        NullRuntimeHost::release_review_attempt(self, request)
    }

    fn record_reviewer_invocation(
        &self,
        request: &ReviewerInvocationRequest,
    ) -> Result<Option<u64>, OrbitError> {
        NullRuntimeHost::record_reviewer_invocation(self, request)
    }

    fn review_report_correction(
        &self,
        request: &ReviewReportCorrectionRequest,
    ) -> Result<Option<String>, OrbitError> {
        NullRuntimeHost::review_report_correction(self, request)
    }

    fn review_report_evidence_hold(
        &self,
        request: &ReviewReportCorrectionRequest,
    ) -> Result<Option<String>, OrbitError> {
        NullRuntimeHost::review_report_evidence_hold(self, request)
    }

    fn handoff_landing_context(
        &self,
        handoff_id: &str,
    ) -> Result<HandoffLandingContext, OrbitError> {
        NullRuntimeHost::handoff_landing_context(self, handoff_id)
    }

    fn record_handoff_landing(&self, update: &HandoffLandingUpdate) -> Result<(), OrbitError> {
        NullRuntimeHost::record_handoff_landing(self, update)
    }

    fn record_handoff_landing_wait(
        &self,
        handoff_id: &str,
        reason: &str,
    ) -> Result<(), OrbitError> {
        NullRuntimeHost::record_handoff_landing_wait(self, handoff_id, reason)
    }

    fn record_handoff_landing_history(
        &self,
        handoff_id: &str,
        event: &str,
        note: &str,
    ) -> Result<(), OrbitError> {
        NullRuntimeHost::record_handoff_landing_history(self, handoff_id, event, note)
    }

    fn record_handoff_landing_completion_hold(
        &self,
        handoff_id: &str,
        history: &orbit_types::task::TaskHistoryEntry,
        comment: &str,
    ) -> Result<(), OrbitError> {
        NullRuntimeHost::record_handoff_landing_completion_hold(self, handoff_id, history, comment)
    }

    fn local_machine_id(&self) -> Option<String> {
        NullRuntimeHost::local_machine_id(self)
    }

    fn kept_claim_candidate(
        &self,
        task_id: &str,
    ) -> Result<Option<KeptClaimCandidate>, OrbitError> {
        NullRuntimeHost::kept_claim_candidate(self, task_id)
    }

    fn claim_execution_context(&self) -> Result<ClaimExecutionContext, OrbitError> {
        NullRuntimeHost::claim_execution_context(self)
    }

    fn attach_claim_validation_log(&self, path: &str, content: Vec<u8>) -> Result<(), OrbitError> {
        NullRuntimeHost::attach_claim_validation_log(self, path, content)
    }

    fn record_claim_handoff(
        &self,
        handoff: &orbit_types::workflow::handoff::TaskHandoff,
    ) -> Result<(), OrbitError> {
        NullRuntimeHost::record_claim_handoff(self, handoff)
    }

    fn attach_task_validation_log(
        &self,
        task_id: &str,
        run_id: &str,
        path: &str,
        content: Vec<u8>,
    ) -> Result<(), OrbitError> {
        NullRuntimeHost::attach_task_validation_log(self, task_id, run_id, path, content)
    }

    fn record_flaky_validation(
        &self,
        task_id: &str,
        record: &orbit_types::workflow::FlakyValidation,
    ) -> Result<(), OrbitError> {
        NullRuntimeHost::record_flaky_validation(self, task_id, record)
    }
}
