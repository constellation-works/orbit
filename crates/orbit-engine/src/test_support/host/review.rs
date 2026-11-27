//! Defaults for test fixtures implementing the ReviewGate capability.

#[doc(hidden)]
#[macro_export]
macro_rules! __orbit_test_host_review {
    ($($implementation:tt)*) => {
        $crate::__orbit_test_host_impl! {
            $($implementation)*;
            fn authorize_step_recovery(
                &self,
                _run_id: &str,
                _step_id: &str,
            ) -> Result<$crate::StepRecoveryAdmission, fixture::OrbitError>
            {
                Ok($crate::StepRecoveryAdmission::Allowed)
            }

            fn allocate_step_recovery_decision(
                &self,
                _request: &$crate::StepRecoveryDecisionRequest,
            ) -> Result<
                Option<$crate::StepRecoveryDecisionSlot>,
                fixture::OrbitError,
            > {
                Ok(None)
            }

            fn read_step_recovery_decision(
                &self,
                _slot: &$crate::StepRecoveryDecisionSlot,
            ) -> Result<$crate::StepRecoveryDecisionRead, fixture::OrbitError>
            {
                Err(fixture::unsupported_runtime_capability(
                    "read_step_recovery_decision",
                ))
            }

            fn validate_step_recovery_mutation(
                &self,
                _run_id: &str,
                _step_id: &str,
                _task_ids: &[String],
                _workspace_path: &std::path::Path,
            ) -> Result<(), fixture::OrbitError> {
                Err(fixture::unsupported_runtime_capability(
                    "validate_step_recovery_mutation",
                ))
            }

            fn source_repair_admission(
                &self,
                _run_id: &str,
            ) -> Result<
                Option<fixture::orbit_types::workflow::SourceRepairAdmission>,
                fixture::OrbitError,
            > {
                Ok(None)
            }

            fn owner_published_candidate(
                &self,
                _task_id: &str,
                _source_run_id: Option<&str>,
            ) -> Result<
                Option<fixture::orbit_types::workflow::SourceRepairBinding>,
                fixture::OrbitError,
            > {
                Ok(None)
            }

            fn owner_publication_binding(
                &self,
                _run_id: &str,
            ) -> Result<
                Option<fixture::orbit_types::workflow::SourceRepairBinding>,
                fixture::OrbitError,
            > {
                Ok(None)
            }

            fn validate_source_repair(
                &self,
                _run_id: &str,
            ) -> Result<(), fixture::OrbitError> {
                Err(fixture::OrbitError::CapabilityDenied(
                    "operator source repair is unavailable on this host".into(),
                ))
            }

            fn admit_pr_auto_fix(
                &self,
                _request: &$crate::PrAutoFixRequest,
            ) -> Result<$crate::PrAutoFixAdmission, fixture::OrbitError> {
                Ok($crate::PrAutoFixAdmission::Refused(
                    "this host keeps no auto-fix ledger".to_string(),
                ))
            }

            fn admit_ci_rerun(
                &self,
                _task_ids: &[String],
                _handoff_id: Option<&str>,
                _plan: &fixture::orbit_types::workflow::CiRerunPlan,
                _now_ms: u64,
            ) -> Result<
                fixture::orbit_types::workflow::CiRerunAdmission,
                fixture::OrbitError,
            > {
                Ok(fixture::orbit_types::workflow::CiRerunAdmission::Refused(
                    fixture::orbit_types::workflow::CiRerunRefusal::LedgerAbsent,
                ))
            }

            fn admit_pr_infrastructure(
                &self,
                _execution: &str,
                _task_ids: &[String],
                _evidence: &fixture::orbit_types::workflow::PrInfrastructureEvidence,
            ) -> Result<
                fixture::orbit_types::workflow::PrInfrastructureAdmission,
                fixture::OrbitError,
            > {
                Ok(
                    fixture::orbit_types::workflow::PrInfrastructureAdmission::Refused(
                        "this host keeps no infrastructure ledger".into(),
                    ),
                )
            }

            fn admit_handoff_base_update(
                &self,
                _handoff_id: &str,
                _request: &fixture::orbit_types::workflow::PrBaseUpdateReservation,
            ) -> Result<
                fixture::orbit_types::workflow::PrBaseUpdateAdmission,
                fixture::OrbitError,
            > {
                use fixture::orbit_types::workflow::{PrBaseUpdateAdmission, PrBaseUpdateRefusal};

                Ok(PrBaseUpdateAdmission::Refused(
                    PrBaseUpdateRefusal::HandoffAuthorityUnavailable,
                ))
            }

            fn admit_pr_base_update(
                &self,
                _task_ids: &[String],
                _request: &fixture::orbit_types::workflow::PrBaseUpdateReservation,
            ) -> Result<
                fixture::orbit_types::workflow::PrBaseUpdateAdmission,
                fixture::OrbitError,
            > {
                Ok(
                    fixture::orbit_types::workflow::PrBaseUpdateAdmission::Refused(
                        fixture::orbit_types::workflow::PrBaseUpdateRefusal::LedgerAbsent,
                    ),
                )
            }

            fn refresh_delivery_pull_request(
                &self,
                _request: &fixture::orbit_types::workflow::DeliveryRefreshRequest,
            ) -> Result<
                fixture::orbit_types::workflow::PullRequestRefreshOutcome,
                fixture::OrbitError,
            > {
                Ok(
                    fixture::orbit_types::workflow::PullRequestRefreshOutcome::Refused {
                        reason: "this host cannot refresh a delivery pull request".to_string(),
                    },
                )
            }

            fn audited_delivery_refreshes(
                &self,
                _task_id: &str,
                _pr_number: &str,
                _head_branch: &str,
                _base_branch: &str,
                _published: &str,
                _reported: &str,
            ) -> Result<Vec<String>, fixture::OrbitError> {
                Ok(Vec::new())
            }

            fn record_review_landing(
                &self,
                _request: &$crate::ReviewLandingRequest,
            ) -> Result<(), fixture::OrbitError> {
                Ok(())
            }

            fn release_review_attempt(
                &self,
                _request: &$crate::ReviewReleaseRequest,
            ) -> Result<(), fixture::OrbitError> {
                Ok(())
            }

            fn record_reviewer_invocation(
                &self,
                _request: &$crate::ReviewerInvocationRequest,
            ) -> Result<Option<u64>, fixture::OrbitError> {
                Ok(None)
            }

            fn review_report_correction(
                &self,
                _request: &$crate::ReviewReportCorrectionRequest,
            ) -> Result<Option<String>, fixture::OrbitError> {
                Ok(None)
            }

            fn review_report_evidence_hold(
                &self,
                _request: &$crate::ReviewReportCorrectionRequest,
            ) -> Result<Option<String>, fixture::OrbitError> {
                Ok(None)
            }

            fn handoff_landing_context(
                &self,
                _handoff_id: &str,
            ) -> Result<$crate::HandoffLandingContext, fixture::OrbitError>
            {
                Err(fixture::unsupported_runtime_capability(
                    "handoff_landing_context",
                ))
            }

            fn record_handoff_landing(
                &self,
                _update: &$crate::HandoffLandingUpdate,
            ) -> Result<(), fixture::OrbitError> {
                Err(fixture::unsupported_runtime_capability(
                    "record_handoff_landing",
                ))
            }

            fn record_handoff_landing_wait(
                &self,
                _handoff_id: &str,
                _reason: &str,
            ) -> Result<(), fixture::OrbitError> {
                Err(fixture::unsupported_runtime_capability(
                    "record_handoff_landing_wait",
                ))
            }

            fn record_handoff_landing_history(
                &self,
                _handoff_id: &str,
                _event: &str,
                _note: &str,
            ) -> Result<(), fixture::OrbitError> {
                Err(fixture::unsupported_runtime_capability(
                    "record_handoff_landing_history",
                ))
            }

            fn record_handoff_landing_completion_hold(
                &self,
                _handoff_id: &str,
                _history: &fixture::orbit_types::task::TaskHistoryEntry,
                _comment: &str,
            ) -> Result<(), fixture::OrbitError> {
                Err(fixture::unsupported_runtime_capability(
                    "record_handoff_landing_completion_hold",
                ))
            }

            fn local_machine_id(&self) -> Option<String> {
                None
            }

            fn kept_claim_candidate(
                &self,
                _task_id: &str,
            ) -> Result<
                Option<fixture::orbit_store::contract::KeptClaimCandidate>,
                fixture::OrbitError,
            > {
                Ok(None)
            }

            fn claim_execution_context(
                &self,
            ) -> Result<$crate::ClaimExecutionContext, fixture::OrbitError>
            {
                Err(fixture::unsupported_runtime_capability(
                    "claim_execution_context",
                ))
            }

            fn attach_claim_validation_log(
                &self,
                _path: &str,
                _content: Vec<u8>,
            ) -> Result<(), fixture::OrbitError> {
                Err(fixture::unsupported_runtime_capability(
                    "attach_claim_validation_log",
                ))
            }

            fn record_claim_handoff(
                &self,
                _handoff: &fixture::orbit_types::workflow::handoff::TaskHandoff,
            ) -> Result<(), fixture::OrbitError> {
                Err(fixture::unsupported_runtime_capability(
                    "record_claim_handoff",
                ))
            }

            fn attach_task_validation_log(
                &self,
                _task_id: &str,
                _run_id: &str,
                _path: &str,
                _content: Vec<u8>,
            ) -> Result<(), fixture::OrbitError> {
                Err(fixture::unsupported_runtime_capability(
                    "attach_task_validation_log",
                ))
            }

            fn record_flaky_validation(
                &self,
                _task_id: &str,
                _record: &fixture::orbit_types::workflow::FlakyValidation,
            ) -> Result<(), fixture::OrbitError> {
                Err(fixture::unsupported_runtime_capability(
                    "record_flaky_validation",
                ))
            }
        }
    };
}
