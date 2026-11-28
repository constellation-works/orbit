//! Which tag instructions, pilot notes and live experiment arms each activity
//! of a run received, read back from the `activity.tag_instructions_applied`,
//! `activity.pilot_notes_applied` and `activity.experiment_overlay_applied`
//! rows its dispatches wrote.

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_types::workflow::{
    ExperimentOverlayRecord, PilotNoteAuditRecord, TagInstructionRecord, V2AuditEventKind,
};
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

/// One `task_pilot` dispatch's mapped pilot-note evaluations.
#[derive(Clone, Debug, PartialEq)]
pub struct RunPilotNotesDispatch {
    pub at: DateTime<Utc>,
    pub step_id: String,
    pub activity_name: String,
    pub notes: Vec<PilotNoteAuditRecord>,
}

/// The tag instructions, pilot notes and live experiment arms a run's
/// activities received, oldest dispatch first.
///
/// `unavailable` means the run has no v2 audit evidence; `none` means its
/// trail records no dispatch that received any.
#[derive(Clone, Debug, PartialEq)]
pub struct RunTagInstructions {
    pub state: &'static str,
    pub dispatches: Vec<RunTagInstructionDispatch>,
    pub experiment_overlays: Vec<RunExperimentOverlayDispatch>,
    pub pilot_notes: Vec<RunPilotNotesDispatch>,
    pub truncated: bool,
}

impl RunTagInstructions {
    pub fn unavailable() -> Self {
        Self {
            state: "unavailable",
            dispatches: Vec::new(),
            experiment_overlays: Vec::new(),
            pilot_notes: Vec::new(),
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
            "pilot_notes": self.pilot_notes.iter().map(|dispatch| json!({
                "at": dispatch.at.to_rfc3339(),
                "step_id": dispatch.step_id,
                "activity_name": dispatch.activity_name,
                "notes": dispatch.notes,
            })).collect::<Vec<_>>(),
        })
    }

    /// One line per dispatch for the run header, e.g.
    /// `Tag instructions: implement (agent_implement): ui (Orbit default)`,
    /// then one per `task_pilot` pilot-notes dispatch, then one per applied
    /// experiment arm.
    pub fn header_lines(&self) -> Vec<String> {
        let pilot_notes = self.pilot_notes.iter().map(|dispatch| {
            format!(
                "Pilot notes: {} ({}): {}",
                dispatch.step_id,
                dispatch.activity_name,
                dispatch
                    .notes
                    .iter()
                    .map(|note| {
                        let location = match &note.revision {
                            Some(revision) => {
                                format!(
                                    "{} at {}",
                                    note.path,
                                    revision.get(..12).unwrap_or(revision)
                                )
                            }
                            None => note.path.clone(),
                        };
                        if let Some(reason) = &note.disabled_reason {
                            format!("{} ({}, switched off: {})", note.label, location, reason)
                        } else {
                            let sha = note
                                .sha256
                                .as_deref()
                                .map(|sha| sha.get(..12).unwrap_or(sha))
                                .unwrap_or("?");
                            let bytes = note.bytes.unwrap_or(0);
                            format!(
                                "{} ({}, sha256 {}, {} bytes)",
                                note.label, location, sha, bytes
                            )
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        });
        let arms = self.experiment_overlays.iter().flat_map(|dispatch| {
            dispatch.overlays.iter().map(move |overlay| {
                let mut line = format!(
                    "Experiment applied: {} ({}): {} / {} segment {}, digest {}",
                    dispatch.step_id,
                    dispatch.activity_name,
                    overlay.experiment,
                    overlay.arm,
                    overlay.segment,
                    overlay.digest
                );
                for (name, digest) in &overlay.skills {
                    line.push_str(&format!(", skill {name} {digest}"));
                }
                if let Some(memory) = &overlay.memory {
                    let revision = |number: Option<u32>| {
                        number.map_or_else(|| "none".to_string(), |number| number.to_string())
                    };
                    line.push_str(&format!(
                        ", crew memory {} revision {}",
                        memory.model,
                        revision(memory.model_revision)
                    ));
                    if memory.global {
                        line.push_str(&format!(
                            " global revision {}",
                            revision(memory.global_revision)
                        ));
                    }
                    line.push_str(&format!(" {}", memory.digest));
                }
                line
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
            .chain(pilot_notes)
            .chain(arms)
            .collect()
    }
}

impl RuntimeKernel {
    /// The tag instructions, pilot notes and experiment overlays `run_id`'s
    /// activities received, from its audit trail, reading at most
    /// [`MAX_TAG_INSTRUCTION_DISPATCHES`] + 1 rows.
    pub fn collect_run_tag_instructions(
        &self,
        run_id: &str,
    ) -> Result<RunTagInstructions, OrbitError> {
        let workspace_id = self.workspace_id()?;
        let snapshot = self.stores().host_store().read()?;
        let v2 = snapshot.v2_audit();
        let present = v2
            .run_ids_with_events(&workspace_id, &[run_id.to_string()], Some("v2_envelope"))?
            .contains(run_id);
        if !present {
            return Ok(RunTagInstructions::unavailable());
        }
        let mut rows = v2.list(&V2AuditEventFilter {
            workspace_id,
            run_id: Some(run_id.to_string()),
            source: Some("v2_envelope".to_string()),
            body_kinds: vec![
                "tag_instructions_applied".to_string(),
                "pilot_notes_applied".to_string(),
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
        let mut pilot_notes = Vec::new();
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
                V2AuditEventKind::PilotNotesApplied {
                    step_id,
                    activity_name,
                    notes,
                } => pilot_notes.push(RunPilotNotesDispatch {
                    at: event.envelope.ts,
                    step_id,
                    activity_name,
                    notes,
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
            state: if dispatches.is_empty()
                && experiment_overlays.is_empty()
                && pilot_notes.is_empty()
            {
                "none"
            } else {
                "observed"
            },
            dispatches,
            experiment_overlays,
            pilot_notes,
            truncated,
        })
    }
}
