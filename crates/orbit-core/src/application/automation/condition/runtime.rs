use chrono::Utc;
use orbit_common::OrbitError;
use orbit_types::workflow::automation::ConditionAudit;

use super::enable::{ConditionEnablePreview, ConditionStallCleared, enable_preview};
use super::evaluate::{ConditionDefinition, condition_consumer, condition_refusal};
use crate::OrbitRuntime;

impl OrbitRuntime {
    /// The enable dry run of a condition auto-task (§9): what enabling it now
    /// would baseline or fire, and whether `max_matches` refuses it. Writes
    /// nothing.
    pub fn auto_task_enable_preview(
        &self,
        name: &str,
    ) -> Result<ConditionEnablePreview, OrbitError> {
        let definition = self
            .auto_task_show(name)?
            .ok_or_else(|| OrbitError::InvalidInput(format!("no such auto-task '{name}'")))?;
        let condition = ConditionDefinition::of_auto_task(&definition).ok_or_else(|| {
            OrbitError::InvalidInput(format!(
                "auto-task '{name}' has no condition; --dry-run previews condition definitions \
                 only"
            ))
        })?;
        enable_preview(self, &condition, Utc::now())
    }

    /// Clear a condition's stall (§8, §9): the operator's acknowledgement
    /// that a storm, loop or oscillation was looked at. Audited, fenced, and
    /// refused off the owner. The next pass evaluates normally; a cause that
    /// persists stalls it again.
    pub fn clear_condition_stall(
        &self,
        selector: &str,
        reason: &str,
    ) -> Result<ConditionStallCleared, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        if let Some(refusal) = condition_refusal(self) {
            return Err(OrbitError::InvalidInput(format!(
                "clear-stall runs on the workspace owner; this checkout is {refusal}"
            )));
        }
        let reason = reason.trim();
        if reason.is_empty() {
            return Err(OrbitError::InvalidInput(
                "clear-stall needs a reason for the audit record".into(),
            ));
        }
        let found = self.condition_definition_for(selector)?;
        let consumer = condition_consumer(self, found.carrier, &found.name)?;
        let store = self.automation_store()?;
        let state = store.condition_state(&consumer)?.ok_or_else(|| {
            OrbitError::InvalidInput(format!(
                "{} '{}' has no condition state yet; nothing is stalled",
                found.carrier.kind(),
                found.name
            ))
        })?;
        let Some(stall) = state.stall.clone() else {
            return Err(OrbitError::InvalidInput(format!(
                "{} '{}' is not stalled",
                found.carrier.kind(),
                found.name
            )));
        };
        let audit = ConditionAudit {
            consumer: consumer.clone(),
            action: "clear_stall".into(),
            actor: self.actor().resolve_write_label(None, None)?,
            at: Utc::now(),
            reason: reason.to_string(),
            stall: Some(stall.clone()),
        };
        let mut next = state.clone();
        next.stall = None;
        next.generation += 1;
        if !store.condition_commit_audited(&state, &next, &audit)? {
            return Err(OrbitError::InvalidInput(
                "the condition moved while clearing; refresh and retry".into(),
            ));
        }
        Ok(ConditionStallCleared {
            consumer,
            cleared: stall,
            audit,
        })
    }
}
