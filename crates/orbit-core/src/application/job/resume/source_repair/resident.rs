//! Task-level admission limits for resident source repair.

use chrono::{Duration, Utc};
use orbit_common::OrbitError;
use orbit_types::task::{
    DecisionAction, DecisionGate, DecisionRequest, DecisionRequestState, DecisionRiskTier,
    TaskStatus,
};
use orbit_types::workflow::{RESIDENT_SOURCE_REPAIR_PROVENANCE, source_repair_reservations};

use super::admission::refuse;
use crate::OrbitRuntime;

fn low_risk(risk: DecisionRiskTier) -> bool {
    matches!(
        risk,
        DecisionRiskTier::Routine | DecisionRiskTier::Reversible
    )
}

fn permits_resident_recovery(request: &DecisionRequest) -> bool {
    request.gate == DecisionGate::BlockTriage
        && request.raised_by == "final_recovery"
        && request.human_action.is_none()
        && !request.answers.is_empty()
        && low_risk(request.risk_tier)
        && request.answers.iter().all(|answer| {
            low_risk(answer.risk_tier)
                && !matches!(
                    answer
                        .option
                        .as_ref()
                        .and_then(|option| option.action.as_ref()),
                    Some(DecisionAction::Archive | DecisionAction::Reject | DecisionAction::Land)
                )
        })
}

impl OrbitRuntime {
    pub(super) fn check_resident_delivery_recovery(
        &self,
        task_id: &str,
        action_key: &str,
    ) -> Result<(), OrbitError> {
        let task = self.get_task(task_id)?;
        if task.status != TaskStatus::Blocked {
            return Err(refuse("resident recovery requires a blocked task"));
        }
        if task
            .tags
            .iter()
            .any(|tag| matches!(tag.as_str(), "security" | "security-review"))
        {
            return Err(refuse(
                "a security task requires an operator to authorize source repair",
            ));
        }
        let entries = self.get_task_history(task_id)?;
        let now = Utc::now();
        let reservations = source_repair_reservations(&entries)
            .map_err(|error| refuse(format!("unreadable repair reservation: {error}")))?;
        if reservations.iter().any(|record| {
            record.authorizer_provenance == RESIDENT_SOURCE_REPAIR_PROVENANCE
                && record.request_key != action_key
                && record.authorized_at > now - Duration::hours(24)
        }) {
            return Err(OrbitError::PolicyDenied(
                "resident_recovery_limit: this task already reserved recovery in the last 24 hours"
                    .into(),
            ));
        }
        let requests = self.task_decision_requests_in(
            &task,
            &entries,
            &self.get_task_comments(task_id)?,
            now,
        )?;
        if let Some(request) = requests.iter().find(|request| {
            request.state == DecisionRequestState::Open && !permits_resident_recovery(request)
        }) {
            return Err(OrbitError::PolicyDenied(format!(
                "resident_recovery_decision_refused: decision {} requires the operator",
                request.id
            )));
        }
        Ok(())
    }
}
