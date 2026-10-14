//! Admission of `orbit run auto --auto-fix` pull-request repairs.
//!
//! The completion watcher asks before it hands a failed required check or a
//! merge conflict to the run's repair pipeline. The answer comes from two
//! durable records: the run's frozen `workflow.low_complexity_crews` draw,
//! which names the crew every repair of the run uses, and the pull request's
//! ledger in the primary task's history, which bounds and deduplicates the
//! dispatches across heads, restarts and runs. Infrastructure prerequisite
//! recovery has a separate one-rerun history reservation under the same owner
//! lock; it draws no crew and never dispatches an agent.
use chrono::Utc;
use orbit_common::OrbitError;
use orbit_engine::{PrAutoFixAdmission, PrAutoFixRequest, RuntimeHost, TaskAutomationUpdate};
use orbit_types::task::{TaskComplexity, TaskHistoryEntry};
use orbit_types::workflow::{
    PR_AUTO_FIX_ADMITTED_EVENT, PR_AUTO_FIX_BUDGET, PR_AUTO_FIX_CREWS_KEY, PrAutoFixClaim,
    PrAutoFixDecision, decide_pr_auto_fix, pr_auto_fix_attempts,
};
use serde_json::json;

use crate::OrbitRuntime;
use crate::application::job::crew_pools::random_crew_ticket;
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
            .complexity_pool_activity_crew(
                &draw_input,
                TaskComplexity::Low,
                &mut random_crew_ticket,
            )
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
            .and_then(|state| {
                state
                    .activity_crew_draws
                    .get(PR_AUTO_FIX_CREWS_KEY)
                    .cloned()
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
                    PR_AUTO_FIX_BUDGET,
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
