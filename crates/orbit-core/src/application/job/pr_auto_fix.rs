//! Admission of `orbit run auto --auto-fix` pull-request repairs.
//!
//! The completion watcher asks before it hands a failed required check or a
//! merge conflict to the run's repair pipeline. The answer comes from two
//! durable records: the run's frozen `workflow.purpose_crews.auto_fix` draw
//! (the low complexity pool while that purpose is unset), which names the
//! crew every repair of the run uses, and the pull request's
//! ledger in the primary task's history, which bounds and deduplicates the
//! dispatches across heads, restarts and runs. Infrastructure prerequisite
//! recovery has a separate one-rerun history reservation under the same owner
//! lock; it draws no crew and never dispatches an agent.
use chrono::Utc;
use orbit_common::OrbitError;
use orbit_engine::{PrAutoFixAdmission, PrAutoFixRequest, RuntimeHost, TaskAutomationUpdate};
use orbit_types::task::{TaskComplexity, TaskHistoryEntry};
use orbit_types::workflow::{
    CrewPurpose, PR_AUTO_FIX_ADMITTED_EVENT, PR_AUTO_FIX_BUDGET, PR_AUTO_FIX_CREWS_KEY,
    PrAutoFixClaim, PrAutoFixDecision, decide_pr_auto_fix, pr_auto_fix_attempts,
};
use serde_json::json;

use crate::OrbitRuntime;
use crate::application::job::crew_pools::{complexity_crews_key, random_crew_ticket};
use crate::runtime::engine::crew::enforce_crew_allowlist;

impl OrbitRuntime {
    /// Decide one auto-fix repair, recording it when admitted.
    ///
    /// The crew is drawn first, through the same frozen per-run draw the
    /// repair activity's dispatch reads, so the recorded crew is the one that
    /// runs. A pool the run's allowlist, the configured crews or this host
    /// cannot serve refuses the repair; nothing escalates to another pool.
    pub(crate) fn admit_pr_auto_fix(
        &self,
        request: &PrAutoFixRequest,
    ) -> Result<PrAutoFixAdmission, OrbitError> {
        // A claimed worker writes its owner's record only through the typed
        // handoff, so the ledger lives on the owning host.
        if self.worker_invocation().is_some() {
            return Ok(PrAutoFixAdmission::Refused(
                "auto-fix repairs run on the host that owns the task".to_string(),
            ));
        }
        let Some(primary) = request.task_ids.iter().min() else {
            return Ok(PrAutoFixAdmission::Refused(
                "the pull request names no task to keep its auto-fix ledger on".to_string(),
            ));
        };
        let draw_input = json!({
            "job_run_id": request.run_id,
            "allowed_crews": request.allowed_crews,
        });
        let crew = match self
            .purpose_activity_crew(&draw_input, CrewPurpose::AutoFix, &mut random_crew_ticket)
            .and_then(|crew| {
                let allowlist = self.crew_allowlist_from_input(&draw_input)?;
                let origin = format!("`{PR_AUTO_FIX_CREWS_KEY}`");
                enforce_crew_allowlist(allowlist.as_ref(), &crew, &origin)?;
                Ok(crew)
            }) {
            Ok(crew) => crew,
            Err(error) => {
                return Ok(PrAutoFixAdmission::Refused(format!(
                    "no repair crew can be drawn from `{PR_AUTO_FIX_CREWS_KEY}`: {error}"
                )));
            }
        };
        let crew_source = self
            .stores()
            .jobs()
            .read_run_state(&request.run_id)?
            .and_then(|mut state| {
                // A run admitted before the auto-fix purpose froze its draw
                // under the low complexity pool's key.
                let legacy = complexity_crews_key(TaskComplexity::Low);
                state
                    .activity_crew_draws
                    .remove(PR_AUTO_FIX_CREWS_KEY)
                    .or_else(|| state.activity_crew_draws.remove(&legacy))
            })
            .map_or_else(|| PR_AUTO_FIX_CREWS_KEY.to_string(), |draw| draw.source);

        let claim = PrAutoFixClaim {
            pr_number: &request.pr_number,
            head_sha: &request.head_sha,
            blocker: request.blocker,
            run_id: &request.run_id,
        };
        let mut decision = None;
        self.stores()
            .tasks()
            .with_task_write_lock(primary, &mut || {
                let history = self.get_task_history(primary)?;
                let attempts = pr_auto_fix_attempts(&history, &request.pr_number);
                let decided = decide_pr_auto_fix(
                    &attempts,
                    &claim,
                    PR_AUTO_FIX_BUDGET.saturating_sub(
                        orbit_types::workflow::source_repair_reservations(&history)
                            .map_err(|e| {
                                OrbitError::JobValidation(format!(
                                    "unreadable source repair ledger: {e}"
                                ))
                            })?
                            .iter()
                            .filter(|record| record.binding.pull_request == request.pr_number)
                            .count() as u32,
                    ),
                    &crew.name,
                    &crew_source,
                );
                if let PrAutoFixDecision::Admit(attempt) = &decided {
                    let note = serde_json::to_string(attempt).map_err(|error| {
                        OrbitError::Execution(format!(
                            "auto-fix attempt could not be recorded: {error}"
                        ))
                    })?;
                    self.apply_task_automation_update(
                        primary,
                        TaskAutomationUpdate {
                            append_history: vec![TaskHistoryEntry {
                                at: Utc::now(),
                                by: "system".to_string(),
                                event: PR_AUTO_FIX_ADMITTED_EVENT.to_string(),
                                note: Some(note),
                                from_status: None,
                                to_status: None,
                                operation_id: None,
                                decision: None,
                            }],
                            ..TaskAutomationUpdate::default()
                        },
                    )?;
                }
                decision = Some(decided);
                Ok(())
            })?;
        let decision = decision.ok_or_else(|| {
            OrbitError::Execution("auto-fix admission did not run under the task lock".to_string())
        })?;
        Ok(PrAutoFixAdmission::Decided(decision))
    }
}

impl OrbitRuntime {
    /// Reserve infrastructure recovery without drawing or invoking a crew.
    pub(crate) fn admit_pr_infrastructure(
        &self,
        execution: &str,
        task_ids: &[String],
        evidence: &orbit_types::workflow::PrInfrastructureEvidence,
    ) -> Result<orbit_types::workflow::PrInfrastructureAdmission, OrbitError> {
        use orbit_types::workflow::{
            PR_INFRASTRUCTURE_RESERVED_EVENT, PrInfrastructureAdmission, decide_pr_infrastructure,
        };
        if self.worker_invocation().is_some() {
            return Ok(PrInfrastructureAdmission::Refused(
                "infrastructure recovery requires the owning host".into(),
            ));
        }
        let Some(primary) = task_ids.iter().min() else {
            return Ok(PrInfrastructureAdmission::Refused(
                "no owning task for the infrastructure ledger".into(),
            ));
        };
        let mut decision = None;
        self.stores()
            .tasks()
            .with_task_write_lock(primary, &mut || {
                let history = self.get_task_history(primary)?;
                let admitted = decide_pr_infrastructure(&history, execution, evidence);
                if let PrInfrastructureAdmission::Reserved(reservation) = &admitted {
                    let note = serde_json::to_string(reservation)
                        .map_err(|error| OrbitError::Execution(error.to_string()))?;
                    self.apply_task_automation_update(
                        primary,
                        TaskAutomationUpdate {
                            append_history: vec![TaskHistoryEntry {
                                at: Utc::now(),
                                by: "system".into(),
                                event: PR_INFRASTRUCTURE_RESERVED_EVENT.into(),
                                note: Some(note),
                                from_status: None,
                                to_status: None,
                                operation_id: None,
                                decision: None,
                            }],
                            ..TaskAutomationUpdate::default()
                        },
                    )?;
                }
                decision = Some(admitted);
                Ok(())
            })?;
        decision.ok_or_else(|| {
            OrbitError::Execution(
                "infrastructure admission did not execute under the task lock".into(),
            )
        })
    }
}

impl OrbitRuntime {
    /// Persist before any base-update mutation, using the same owner task lock
    /// for inherited failures and missing workflow emission.
    pub(crate) fn admit_pr_base_update(
        &self,
        task_ids: &[String],
        request: &orbit_types::workflow::PrBaseUpdateReservation,
    ) -> Result<orbit_types::workflow::PrBaseUpdateAdmission, OrbitError> {
        self.reserve_pr_base_update(task_ids, request)
    }

    pub(crate) fn admit_handoff_base_update(
        &self,
        handoff_id: &str,
        request: &orbit_types::workflow::PrBaseUpdateReservation,
    ) -> Result<orbit_types::workflow::PrBaseUpdateAdmission, OrbitError> {
        let (claim, accepted) = self.landing_claim(handoff_id)?;
        if request.execution != handoff_id
            || accepted.handoff.candidate.repository != request.repository
            || accepted.handoff.candidate.source_branch != request.head_branch
            || accepted.handoff.candidate.base_branch != request.base_branch
            || accepted.handoff.candidate.candidate.commit != request.head_sha
            || accepted.handoff.candidate.delivery
                != (orbit_types::workflow::handoff::HandoffDelivery::PullRequest {
                    number: request
                        .pr_number
                        .parse()
                        .map_err(|_| OrbitError::InvalidInput("invalid base update PR".into()))?,
                })
        {
            return Err(OrbitError::InvalidInput(
                "base update differs from accepted handoff".into(),
            ));
        }
        let observation = orbit_store::contracts::HandoffObservation {
            candidate: accepted.handoff.candidate.clone(),
            footprint_widening: accepted.handoff.footprint_widening.clone(),
            required_commands: self.workflow_required_validation_commands().to_vec(),
            owner_completion_authority: self.owner_completion_authority(),
            review: None,
            known_red: vec![],
            // Approval and landing recompute this owner's judgement of the
            // accepted candidate. The base-update recheck has to do the same,
            // or a skip-validation or skip-review handoff that acceptance
            // already authorized is refused before a provider update is reserved.
            light_delivery: self
                .owner_light_delivery(&accepted.handoff.task_id, &accepted.handoff.candidate)?,
        };
        let auth = orbit_store::contracts::ClaimInvocation::trusted_operator(
            claim.claim.task_id.clone(),
            claim.claim.claim_id,
            "owner-base-update".into(),
        )
        .with_handoff_observation(observation);
        self.admission_boundary()?
            .reserve_handoff_base_update(&auth, request)
    }

    fn reserve_pr_base_update(
        &self,
        task_ids: &[String],
        request: &orbit_types::workflow::PrBaseUpdateReservation,
    ) -> Result<orbit_types::workflow::PrBaseUpdateAdmission, OrbitError> {
        use orbit_types::workflow::{
            PR_BASE_UPDATE_RESERVED_EVENT, PrBaseUpdateAdmission, decide_pr_base_update,
        };
        if self.worker_invocation().is_some() {
            return Ok(PrBaseUpdateAdmission::Refused(
                "base updates require the owning host".into(),
            ));
        }
        let primary = task_ids
            .iter()
            .min()
            .ok_or_else(|| OrbitError::InvalidInput("no owning task for base update".into()))?;
        let mut decision = None;
        self.stores()
            .tasks()
            .with_task_write_lock(primary, &mut || {
                let admitted = decide_pr_base_update(&self.get_task_history(primary)?, request);
                if matches!(admitted, PrBaseUpdateAdmission::Reserved) {
                    self.apply_task_automation_update(
                        primary,
                        TaskAutomationUpdate {
                            append_history: vec![TaskHistoryEntry {
                                at: Utc::now(),
                                by: "system".into(),
                                event: PR_BASE_UPDATE_RESERVED_EVENT.into(),
                                note: Some(
                                    serde_json::to_string(request)
                                        .map_err(|e| OrbitError::Execution(e.to_string()))?,
                                ),
                                from_status: None,
                                to_status: None,
                                operation_id: None,
                                decision: None,
                            }],
                            ..Default::default()
                        },
                    )?;
                }
                decision = Some(admitted);
                Ok(())
            })?;
        decision.ok_or_else(|| {
            OrbitError::Execution("base update admission did not run under the task lock".into())
        })
    }
}
