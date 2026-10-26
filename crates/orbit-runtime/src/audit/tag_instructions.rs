//! Which tag instructions and live experiment arms each activity of a run
//! received, read back from the `activity.tag_instructions_applied` and
//! `activity.experiment_overlay_applied` rows its dispatches wrote.

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_types::workflow::{ExperimentOverlayRecord, TagInstructionRecord, V2AuditEventKind};
use serde_json::{Value, json};

use crate::RuntimeKernel;
use orbit_store::V2AuditEventFilter;

/// Most activity dispatches one read lists. A run dispatches a handful of
/// agents; past this the projection says it was cut.
pub const MAX_TAG_INSTRUCTION_DISPATCHES: usize = 32;

/// One dispatch's applied tag instructions.
#[derive(Clone, Debug, PartialEq)]
pub struct RunTagInstructionDispatch {
    pub at: DateTime<Utc>,
    pub step_id: String,
    pub activity_name: String,
    pub task_id: String,
    pub instructions: Vec<TagInstructionRecord>,
}

/// One dispatch's applied live experiment arms, each with the digest of what
/// it received.
#[derive(Clone, Debug, PartialEq)]
pub struct RunExperimentOverlayDispatch {
    pub at: DateTime<Utc>,
    pub step_id: String,
    pub activity_name: String,
    pub task_id: String,
    pub overlays: Vec<ExperimentOverlayRecord>,
}

/// The tag instructions and live experiment arms a run's activities
/// received, oldest dispatch first.
///
/// `unavailable` means the run has no v2 audit evidence; `none` means its
/// trail records no dispatch that received any.
#[derive(Clone, Debug, PartialEq)]
pub struct RunTagInstructions {
    pub state: &'static str,
    pub dispatches: Vec<RunTagInstructionDispatch>,
    pub experiment_overlays: Vec<RunExperimentOverlayDispatch>,
    pub truncated: bool,
}

impl RunTagInstructions {
    pub fn unavailable() -> Self {
        Self {
            state: "unavailable",
            dispatches: Vec::new(),
            experiment_overlays: Vec::new(),
            truncated: false,
        }
    }

    /// Shared CLI and tool projection.
    pub fn to_json(&self) -> Value {
        json!({
            "state": self.state,
            "limit": MAX_TAG_INSTRUCTION_DISPATCHES,
            "truncated": self.truncated,
            "items": self.dispatches.iter().map(|dispatch| json!({
                "at": dispatch.at.to_rfc3339(),
                "step_id": dispatch.step_id,
                "activity_name": dispatch.activity_name,
                "task_id": dispatch.task_id,
                "instructions": dispatch.instructions,
            })).collect::<Vec<_>>(),
            "experiment_overlays": self.experiment_overlays.iter().map(|dispatch| json!({
                "at": dispatch.at.to_rfc3339(),
                "step_id": dispatch.step_id,
                "activity_name": dispatch.activity_name,
                "task_id": dispatch.task_id,
                "overlays": dispatch.overlays,
            })).collect::<Vec<_>>(),
        })
    }

    /// One line per dispatch for the run header, e.g.
    /// `Tag instructions: implement (agent_implement): ui (Orbit default)`,
    /// then one per applied experiment arm, e.g.
    /// `Experiment applied: implement (agent_implement): exp-1a2b / treatment
    /// segment 1, digest sha256:…`.
    pub fn header_lines(&self) -> Vec<String> {
        let arms = self.experiment_overlays.iter().flat_map(|dispatch| {
            dispatch.overlays.iter().map(move |overlay| {
                format!(
                    "Experiment applied: {} ({}): {} / {} segment {}, digest {}",
                    dispatch.step_id,
                    dispatch.activity_name,
                    overlay.experiment,
                    overlay.arm,
                    overlay.segment,
                    overlay.digest
                )
            })
        });
        self.dispatches
            .iter()
            .map(|dispatch| {
                format!(
                    "Tag instructions: {} ({}): {}",
                    dispatch.step_id,
                    dispatch.activity_name,
                    dispatch
                        .instructions
                        .iter()
                        .map(|instruction| format!(
                            "{} ({}, sha256 {})",
                            instruction.tag,
                            instruction.source.label(),
                            instruction.sha256.get(..12).unwrap_or(&instruction.sha256)
                        ))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })
            .chain(arms)
            .collect()
    }
}

impl RuntimeKernel {
    /// The tag instructions `run_id`'s activities received, from its audit
    /// trail, reading at most [`MAX_TAG_INSTRUCTION_DISPATCHES`] + 1 rows.
    pub fn collect_run_tag_instructions(
        &self,
        run_id: &str,
    ) -> Result<RunTagInstructions, OrbitError> {
        let workspace_id = self.workspace_id()?;
        let store = self.v2_audit_store()?;
        let present = store
            .list_v2_audit_run_ids_with_events(
                &workspace_id,
                &[run_id.to_string()],
                Some("v2_envelope"),
            )?
            .contains(run_id);
        if !present {
            return Ok(RunTagInstructions::unavailable());
        }
        let mut rows = store.list_v2_audit_events(&V2AuditEventFilter {
            workspace_id,
            run_id: Some(run_id.to_string()),
            source: Some("v2_envelope".to_string()),
            body_kinds: vec![
                "tag_instructions_applied".to_string(),
                "experiment_overlay_applied".to_string(),
            ],
            oldest_first: true,
            limit: Some(MAX_TAG_INSTRUCTION_DISPATCHES + 1),
            ..Default::default()
        })?;
        let truncated = rows.len() > MAX_TAG_INSTRUCTION_DISPATCHES;
        rows.truncate(MAX_TAG_INSTRUCTION_DISPATCHES);
        let mut dispatches = Vec::new();
        let mut experiment_overlays = Vec::new();
        for row in rows {
            let Ok(event) =
                serde_json::from_str::<orbit_types::workflow::V2AuditEvent>(&row.payload_json)
            else {
                continue;
            };
            match event.kind {
                V2AuditEventKind::TagInstructionsApplied {
                    step_id,
                    activity_name,
                    task_id,
                    instructions,
                } => dispatches.push(RunTagInstructionDispatch {
                    at: event.envelope.ts,
                    step_id,
                    activity_name,
                    task_id,
                    instructions,
                }),
                V2AuditEventKind::ExperimentOverlayApplied {
                    step_id,
                    activity_name,
                    task_id,
                    overlays,
                } => experiment_overlays.push(RunExperimentOverlayDispatch {
                    at: event.envelope.ts,
                    step_id,
                    activity_name,
                    task_id,
                    overlays,
                }),
                _ => {}
            }
        }
        Ok(RunTagInstructions {
            state: if dispatches.is_empty() && experiment_overlays.is_empty() {
                "none"
            } else {
                "observed"
            },
            dispatches,
            experiment_overlays,
            truncated,
        })
    }
}
