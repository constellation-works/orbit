//! Grant-bound promotion of fresh, positively assessed proposed work
//! [ORB-11332].
//!
//! Promotion is deterministic and evidence-driven: the shared preparation
//! consumer's accepted assessment must be positive and bound to the task's
//! *current* material fingerprint at the *current* landing revision, the
//! task's dependencies must be satisfied, and the grant must carry the
//! promote right with an automatic promotion preference. Populated selectors,
//! wrapper success, or an older assessment are not readiness. The write
//! happens under the task lock after a fresh recheck, and every decision is
//! recorded so "why did this task start?" has a durable answer.

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_config::PromotionPreference;
use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
use orbit_types::task::{TaskReferenceIndex, TaskStatus, task_dependencies_ready_with_index};
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::workflow::OperationGrant;
use serde::Serialize;
use serde_json::json;

use super::{PROMOTION_AUDIT, captured_policy};
use crate::OrbitRuntime;
use crate::application::automation::preparation::{self, InstructionSnapshot};

/// History event recorded on a task promoted under a grant.
pub(crate) const PROMOTION_EVENT: &str = "operation_promoted";

/// One promotion decision for a task in the grant scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct PromotionDecision {
    pub task_id: String,
    /// `promoted` or `withheld`.
    pub outcome: &'static str,
    /// Why it was withheld, or `fresh_positive_assessment` when promoted.
    pub reason: &'static str,
}

impl PromotionDecision {
    fn withheld(task_id: &str, reason: &'static str) -> Self {
        Self {
            task_id: task_id.to_string(),
            outcome: "withheld",
            reason,
        }
    }
}

/// Promote every proposed task in `grant`'s scope that has fresh positive
/// evidence. Returns one decision per proposed task; tasks that are no
/// longer proposed are not promotion candidates and are omitted.
pub(crate) fn promote_within_grant(
    runtime: &OrbitRuntime,
    grant: &OperationGrant,
) -> Result<Vec<PromotionDecision>, OrbitError> {
    let mut decisions = Vec::new();
    let now = Utc::now();
    if !grant.admission(now).admits() {
        return Ok(decisions);
    }
    let policy = captured_policy(grant)?;
    let promotion_allowed =
        grant.rights.promote && policy.promotion.value == PromotionPreference::Automatic;

    let status_by_id = runtime
        .list_tasks()?
        .into_iter()
        .map(|task| (task.id, task.status))
        .collect();
    let reference_index = TaskReferenceIndex::from_status_index(&status_by_id);
    let branch = runtime.workflow_base_branch().to_string();
    let mut source: Option<(String, InstructionSnapshot)> = None;

    for task_id in &grant.task_ids {
        let task = match runtime.get_task(task_id) {
            Ok(task) => task,
            Err(OrbitError::NotFound { .. }) => {
                decisions.push(PromotionDecision::withheld(task_id, "task_missing"));
                continue;
            }
            Err(error) => return Err(error),
        };
        if task.status != TaskStatus::Proposed {
            continue;
        }
        if !promotion_allowed {
            decisions.push(PromotionDecision::withheld(
                task_id,
                if grant.rights.promote {
                    "promotion_separate_approval"
                } else {
                    "promote_right_missing"
                },
            ));
            continue;
        }
        if !orbit_automation::members::preparation::eligible(&task) {
            decisions.push(PromotionDecision::withheld(
                task_id,
                "special_disposition_withheld",
            ));
            continue;
        }
        if !task_dependencies_ready_with_index(&task, &status_by_id, &reference_index) {
            decisions.push(PromotionDecision::withheld(task_id, "unmet_dependency"));
            continue;
        }
        let Some(assessment) = preparation::accepted_assessment(runtime, task_id)? else {
            decisions.push(PromotionDecision::withheld(task_id, "assessment_missing"));
            continue;
        };
        if !assessment.ready {
            decisions.push(PromotionDecision::withheld(task_id, "assessment_unready"));
            continue;
        }
        let (revision, instructions) = match &source {
            Some(source) => (&source.0, &source.1),
            None => {
                let revision = preparation::head_revision(runtime, &branch)?;
                let instructions = preparation::instructions(runtime, &revision)
                    .map_err(orbit_automation::automation_error_to_orbit)?;
                source = Some((revision, instructions));
                let Some(source) = source.as_ref() else {
                    return Err(OrbitError::Execution(
                        "operation promotion did not retain its source snapshot".to_string(),
                    ));
                };
                (&source.0, &source.1)
            }
        };
        let fingerprint =
            preparation::fingerprint_with_instructions(runtime, &task, revision, instructions)
                .map_err(orbit_automation::automation_error_to_orbit)?;
        if fingerprint != assessment.resulting_fingerprint {
            decisions.push(PromotionDecision::withheld(task_id, "assessment_stale"));
            continue;
        }

        let decision = promote_locked(
            runtime,
            grant,
            task_id,
            revision,
            instructions,
            &fingerprint,
        )?;
        runtime.record_pipeline_audit(
            PROMOTION_AUDIT,
            None,
            Some("system"),
            AuditEventStatus::Success,
            json!({
                "grant_id": grant.id,
                "grant_revision": grant.revision,
                "task_id": task_id,
                "outcome": decision.outcome,
                "reason": decision.reason,
                "source_revision": revision,
                "assessment_receipt": assessment.receipt_id,
                "recorded_at": now.to_rfc3339(),
            }),
            None,
        )?;
        decisions.push(decision);
    }
    Ok(decisions)
}

/// Recheck and write under the task lock: the task must still be proposed
/// with the same material fingerprint, and the grant must still admit.
fn promote_locked(
    runtime: &OrbitRuntime,
    grant: &OperationGrant,
    task_id: &str,
    revision: &str,
    instructions: &InstructionSnapshot,
    expected_fingerprint: &str,
) -> Result<PromotionDecision, OrbitError> {
    let mut decision = None;
    let mut operation = || {
        let current = runtime.get_task(task_id)?;
        if current.status != TaskStatus::Proposed {
            decision = Some(PromotionDecision::withheld(task_id, "status_changed"));
            return Ok(());
        }
        let fingerprint =
            preparation::fingerprint_with_instructions(runtime, &current, revision, instructions)
                .map_err(orbit_automation::automation_error_to_orbit)?;
        if fingerprint != expected_fingerprint {
            decision = Some(PromotionDecision::withheld(task_id, "assessment_stale"));
            return Ok(());
        }
        let live = runtime.operation_grant(&grant.id)?;
        if live.revision != grant.revision || !live.admission(Utc::now()).admits() {
            decision = Some(PromotionDecision::withheld(
                task_id,
                "grant_no_longer_admits",
            ));
            return Ok(());
        }
        runtime.apply_task_automation_update(
            task_id,
            TaskAutomationUpdate {
                status: Some(TaskStatus::Backlog),
                status_event: Some(PROMOTION_EVENT.to_string()),
                status_note: Some(format!(
                    "operation-mode promotion under grant {}: fresh positive task-pilot \
                     assessment at {revision}; promoted proposed work to backlog.",
                    grant.id
                )),
                ..TaskAutomationUpdate::default()
            },
        )?;
        decision = Some(PromotionDecision {
            task_id: task_id.to_string(),
            outcome: "promoted",
            reason: "fresh_positive_assessment",
        });
        Ok(())
    };
    runtime
        .stores()
        .tasks()
        .with_task_write_lock(task_id, &mut operation)?;
    decision.ok_or_else(|| {
        OrbitError::Execution("operation promotion did not run under the task lock".to_string())
    })
}
