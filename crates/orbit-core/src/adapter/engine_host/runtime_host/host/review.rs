//! [`ReviewGate`] for [`EngineHost`]: each method delegates to a sibling
//! helper grouped by concern, or adapts a type on the way through.

use orbit_common::OrbitError;
use orbit_engine::{
    ReviewGate, StepRecoveryDecisionRead, StepRecoveryDecisionRequest, StepRecoveryDecisionSlot,
};

use crate::OrbitRuntime;
use crate::adapter::engine_host::runtime_host::{claims, recovery, validation};

use super::EngineHost;

impl ReviewGate for EngineHost<'_> {
    /// Orbit admits every step recovery here. Each one stays bounded by its
    /// step's attempt budget, and any Git mutation it asks for must first pass
    /// `validate_step_recovery_mutation`.
    fn authorize_step_recovery(
        &self,
        run_id: &str,
        step_id: &str,
    ) -> Result<orbit_engine::StepRecoveryAdmission, OrbitError> {
        recovery::authorize_step_recovery(self.runtime, run_id, step_id)
    }

    fn validate_step_recovery_mutation(
        &self,
        run_id: &str,
        step_id: &str,
        task_ids: &[String],
        workspace_path: &std::path::Path,
    ) -> Result<(), OrbitError> {
        recovery::validate_step_recovery_mutation(
            self.runtime,
            run_id,
            step_id,
            task_ids,
            workspace_path,
        )
    }

    fn record_review_landing(
        &self,
        request: &orbit_engine::ReviewLandingRequest,
    ) -> Result<(), OrbitError> {
        crate::application::review::record_review_landing(self.runtime, request)
    }

    fn release_review_attempt(
        &self,
        request: &orbit_engine::ReviewReleaseRequest,
    ) -> Result<(), OrbitError> {
        crate::application::review::release_review_attempt(self.runtime, request)
    }

    fn record_reviewer_invocation(
        &self,
        request: &orbit_engine::ReviewerInvocationRequest,
    ) -> Result<Option<u64>, OrbitError> {
        crate::application::review::record_reviewer_invocation(self.runtime, request)
    }

    fn review_report_correction(
        &self,
        request: &orbit_engine::ReviewReportCorrectionRequest,
    ) -> Result<Option<String>, OrbitError> {
        crate::application::review::review_report_correction(self.runtime, request)
    }

    fn review_report_evidence_hold(
        &self,
        request: &orbit_engine::ReviewReportCorrectionRequest,
    ) -> Result<Option<String>, OrbitError> {
        crate::application::review::review_report_evidence_hold(self.runtime, request)
    }

    fn handoff_landing_context(
        &self,
        handoff_id: &str,
    ) -> Result<orbit_engine::HandoffLandingContext, OrbitError> {
        OrbitRuntime::handoff_landing_context(self.runtime, handoff_id)
    }

    fn record_handoff_landing(
        &self,
        update: &orbit_engine::HandoffLandingUpdate,
    ) -> Result<(), OrbitError> {
        OrbitRuntime::record_handoff_landing(self.runtime, update)
    }

    fn record_handoff_landing_wait(
        &self,
        handoff_id: &str,
        reason: &str,
    ) -> Result<(), OrbitError> {
        OrbitRuntime::record_handoff_landing_wait(self.runtime, handoff_id, reason)
    }

    fn record_handoff_landing_history(
        &self,
        handoff_id: &str,
        event: &str,
        note: &str,
    ) -> Result<(), OrbitError> {
        OrbitRuntime::record_handoff_landing_history(self.runtime, handoff_id, event, note)
    }

    fn record_handoff_landing_completion_hold(
        &self,
        handoff_id: &str,
        history: &orbit_types::task::TaskHistoryEntry,
        comment: &str,
    ) -> Result<(), OrbitError> {
        OrbitRuntime::record_handoff_landing_completion_hold(
            self.runtime,
            handoff_id,
            history,
            comment,
        )
    }

    fn local_machine_id(&self) -> Option<String> {
        self.runtime
            .automation_machine_identity()
            .map(str::to_string)
    }

    fn kept_claim_candidate(
        &self,
        task_id: &str,
    ) -> Result<Option<orbit_store::contract::KeptClaimCandidate>, OrbitError> {
        claims::kept_claim_candidate(self.runtime, task_id)
    }

    fn claim_execution_context(&self) -> Result<orbit_engine::ClaimExecutionContext, OrbitError> {
        claims::claim_execution_context(self.runtime)
    }

    fn attach_claim_validation_log(&self, path: &str, content: Vec<u8>) -> Result<(), OrbitError> {
        validation::attach_claim_validation_log(self.runtime, path, content)
    }

    fn attach_task_validation_log(
        &self,
        task_id: &str,
        run_id: &str,
        path: &str,
        content: Vec<u8>,
    ) -> Result<(), OrbitError> {
        validation::attach_task_validation_log(self.runtime, task_id, run_id, path, content)
    }

    fn record_flaky_validation(
        &self,
        task_id: &str,
        record: &orbit_types::workflow::FlakyValidation,
    ) -> Result<(), OrbitError> {
        validation::record_flaky_validation(self.runtime, task_id, record)
    }

    fn record_claim_handoff(
        &self,
        handoff: &orbit_types::workflow::handoff::TaskHandoff,
    ) -> Result<(), OrbitError> {
        claims::record_claim_handoff(self.runtime, handoff)
    }

    fn allocate_step_recovery_decision(
        &self,
        request: &StepRecoveryDecisionRequest,
    ) -> Result<Option<StepRecoveryDecisionSlot>, OrbitError> {
        recovery::allocate_step_recovery_decision(self.runtime, request)
    }

    fn read_step_recovery_decision(
        &self,
        slot: &StepRecoveryDecisionSlot,
    ) -> Result<StepRecoveryDecisionRead, OrbitError> {
        recovery::read_step_recovery_decision(self.runtime, slot)
    }

    fn source_repair_admission(
        &self,
        run_id: &str,
    ) -> Result<Option<orbit_types::workflow::SourceRepairAdmission>, OrbitError> {
        recovery::source_repair_admission(self.runtime, run_id)
    }

    fn owner_published_candidate(
        &self,
        task_id: &str,
        source_run_id: Option<&str>,
    ) -> Result<Option<orbit_types::workflow::SourceRepairBinding>, OrbitError> {
        recovery::owner_published_candidate(self.runtime, task_id, source_run_id)
    }

    fn owner_publication_binding(
        &self,
        run_id: &str,
    ) -> Result<Option<orbit_types::workflow::SourceRepairBinding>, OrbitError> {
        recovery::owner_publication_binding(self.runtime, run_id)
    }

    fn validate_source_repair(&self, run_id: &str) -> Result<(), OrbitError> {
        recovery::validate_source_repair(self.runtime, run_id)
    }

    fn admit_pr_auto_fix(
        &self,
        request: &orbit_engine::PrAutoFixRequest,
    ) -> Result<orbit_engine::PrAutoFixAdmission, OrbitError> {
        OrbitRuntime::admit_pr_auto_fix(self.runtime, request)
    }

    fn admit_handoff_base_update(
        &self,
        handoff_id: &str,
        request: &orbit_types::workflow::PrBaseUpdateReservation,
    ) -> Result<orbit_types::workflow::PrBaseUpdateAdmission, OrbitError> {
        OrbitRuntime::admit_handoff_base_update(self.runtime, handoff_id, request)
    }

    fn admit_pr_base_update(
        &self,
        task_ids: &[String],
        request: &orbit_types::workflow::PrBaseUpdateReservation,
    ) -> Result<orbit_types::workflow::PrBaseUpdateAdmission, OrbitError> {
        OrbitRuntime::admit_pr_base_update(self.runtime, task_ids, request)
    }

    fn refresh_delivery_pull_request(
        &self,
        request: &orbit_types::workflow::DeliveryRefreshRequest,
    ) -> Result<orbit_types::workflow::PullRequestRefreshOutcome, OrbitError> {
        OrbitRuntime::refresh_run_delivery_pull_request(self.runtime, request)
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
        OrbitRuntime::audited_delivery_refreshes(
            self.runtime,
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
        OrbitRuntime::admit_ci_rerun(self.runtime, task_ids, handoff_id, plan, now_ms)
    }

    fn admit_pr_infrastructure(
        &self,
        execution: &str,
        task_ids: &[String],
        evidence: &orbit_types::workflow::PrInfrastructureEvidence,
    ) -> Result<orbit_types::workflow::PrInfrastructureAdmission, OrbitError> {
        OrbitRuntime::admit_pr_infrastructure(self.runtime, execution, task_ids, evidence)
    }
}
