//! Bounded host execution metadata for delivery checkpoint provenance.

use std::collections::HashMap;

use orbit_store::contracts::V2AuditEventFilter;
use orbit_types::workflow::activity_job::{
    AUDIT_ENVELOPE_SCHEMA_VERSION, V2AuditEvent, V2AuditEventKind,
};
use orbit_types::workflow::{DeliveryEvidenceGap, JobRun, JobRunState, JobRunStep, JobTargetType};

use crate::OrbitRuntime;

use super::observation::DeliveryJobShape;

/// Bound execution metadata independently of retry depth.
const MAX_STEP_AUDIT_EVENTS: usize = 256;

impl OrbitRuntime {
    /// Production v2 runs keep executed steps in the host audit store. Read
    /// only bounded step lifecycle metadata from this workspace/run, never
    /// activity input, provider output, response envelopes or a display's
    /// synthetic terminal-state projection. Checkpoints remain the authority
    /// for commit/landing bytes; these records bind execution and timestamps.
    pub(in crate::application::job::run) fn delivery_execution_records(
        &self,
        run: &JobRun,
        shape: &DeliveryJobShape,
    ) -> Result<JobRun, DeliveryEvidenceGap> {
        let mut executed = run.clone();
        if run
            .steps
            .iter()
            .any(|record| record.target_type == JobTargetType::Activity)
        {
            return Ok(executed);
        }
        let rows = self
            .list_v2_audit_events(V2AuditEventFilter {
                workspace_id: self
                    .workspace_id()
                    .map_err(|_| DeliveryEvidenceGap::OwnershipMismatch)?,
                run_id: Some(run.run_id.clone()),
                source: Some("v2_envelope".into()),
                body_kinds: [
                    "step_started",
                    "step_finished",
                    "step_skipped",
                    "step_denied",
                ]
                .map(str::to_string)
                .to_vec(),
                limit: Some(MAX_STEP_AUDIT_EVENTS + 1),
                ..Default::default()
            })
            .map_err(|_| DeliveryEvidenceGap::StateMissing)?;
        if rows.len() > MAX_STEP_AUDIT_EVENTS {
            return Err(DeliveryEvidenceGap::CheckpointInconsistent);
        }
        let mut records = HashMap::<String, JobRunStep>::new();
        for row in rows.into_iter().rev() {
            let event: V2AuditEvent = serde_json::from_str(&row.payload_json)
                .map_err(|_| DeliveryEvidenceGap::CheckpointInconsistent)?;
            let envelope = &event.envelope;
            if envelope.schema_version != AUDIT_ENVELOPE_SCHEMA_VERSION
                || event.kind.event_type() != envelope.event_type
            {
                return Err(DeliveryEvidenceGap::CheckpointInconsistent);
            }
            if row.agent_identity != crate::application::SYSTEM_AUDIT_IDENTITY
                || envelope.agent_identity != row.agent_identity
                || envelope.run_id != run.run_id.as_str()
                || envelope.run_id != row.run_id
                || envelope.event_id != row.event_id
                || envelope.event_type != row.event_type
                || envelope.ts != row.ts
                || envelope.schema_version != row.schema_version
            {
                return Err(DeliveryEvidenceGap::OwnershipMismatch);
            }
            let (step_id, started, success) = match event.kind {
                V2AuditEventKind::StepStarted { step_id } => (step_id, true, false),
                V2AuditEventKind::StepFinished {
                    step_id, outcome, ..
                } => (step_id, false, outcome == "success"),
                V2AuditEventKind::StepSkipped { step_id, .. }
                | V2AuditEventKind::StepDenied { step_id, .. } => (step_id, false, false),
                _ => return Err(DeliveryEvidenceGap::CheckpointInconsistent),
            };
            let Some(step) = shape
                .commit
                .iter()
                .chain(&shape.landings)
                .find(|step| step.id == step_id)
            else {
                continue;
            };
            if started {
                records.insert(
                    step_id.clone(),
                    JobRunStep {
                        lock_wait_ms: None,
                        step_index: step.index,
                        target_type: JobTargetType::Activity,
                        target_id: step_id,
                        started_at: Some(row.ts),
                        finished_at: None,
                        duration_ms: None,
                        exit_code: None,
                        agent_response_json: None,
                        state: JobRunState::Running,
                        error_code: None,
                        error_message: None,
                    },
                );
            } else if let Some(record) = records.get_mut(&step_id) {
                record.finished_at = Some(row.ts);
                record.state = if success {
                    JobRunState::Success
                } else {
                    JobRunState::Failed
                };
            }
        }
        executed.steps.extend(records.into_values());
        Ok(executed)
    }
}
