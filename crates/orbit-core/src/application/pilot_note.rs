//! Pilot notes: reading committed instruction files mapped under
//! `[workflow.pilot_notes.<label>]` for `task_pilot` dispatches and `orbit
//! doctor` checks.
//!
//! Files are always read from a pinned Git commit (never from a worktree).
//! An unusable file switches off only its own label for that run and records
//! the reason in audit rather than refusing the pilot.

use std::path::Path;

use orbit_types::workflow::{
    PilotNoteAuditRecord, PilotNoteInstruction, PilotNotesResolution, TASK_PILOT_ACTIVITY,
};
use serde_json::Value;

use crate::OrbitRuntime;
use crate::application::task::base_revision::{commit_at, read_blob_at};

impl OrbitRuntime {
    /// Resolve all mapped `[workflow.pilot_notes.<label>]` files for
    /// `activity_name` at the source revision `input` names.
    ///
    /// Returns `None` for any activity other than `task_pilot` or when no
    /// pilot notes are configured, keeping prompts byte-identical and emitting
    /// no audit event. Never fails the pilot.
    pub(crate) fn pilot_notes_for_activity(
        &self,
        activity_name: &str,
        input: &Value,
    ) -> Option<PilotNotesResolution> {
        if activity_name != TASK_PILOT_ACTIVITY {
            return None;
        }
        let settings = self.pilot_note_settings();
        if settings.is_empty() {
            return None;
        }
        let requested_revision = input
            .get("source_revision")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .or_else(|| {
                input
                    .get("inspection_revision")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
            });
        let revision = match requested_revision {
            Some(rev) => match commit_at(&self.paths().repo_root, rev) {
                Ok(commit) => commit,
                Err(reason) => {
                    let records = settings
                        .entries()
                        .map(|(label, path)| {
                            PilotNoteAuditRecord::disabled(
                                label,
                                path,
                                Some(rev.to_string()),
                                reason.clone(),
                            )
                        })
                        .collect();
                    return Some(PilotNotesResolution {
                        applied: Vec::new(),
                        records,
                    });
                }
            },
            None => {
                let records = settings
                    .entries()
                    .map(|(label, path)| {
                        PilotNoteAuditRecord::disabled(
                            label,
                            path,
                            None,
                            "no source revision is available for this pilot run".to_string(),
                        )
                    })
                    .collect();
                return Some(PilotNotesResolution {
                    applied: Vec::new(),
                    records,
                });
            }
        };

        let mut applied = Vec::new();
        let mut records = Vec::new();
        for (label, path) in settings.entries() {
            match resolve_pilot_note_at(&self.paths().repo_root, &revision, label, path) {
                Ok(note) => {
                    records.push(note.audit_record());
                    applied.push(note);
                }
                Err(reason) => {
                    records.push(PilotNoteAuditRecord::disabled(
                        label,
                        path,
                        Some(revision.clone()),
                        reason,
                    ));
                }
            }
        }
        Some(PilotNotesResolution { applied, records })
    }
}

/// Read and admit one mapped pilot-note instruction file from commit `revision`.
pub(crate) fn resolve_pilot_note_at(
    repo_root: &Path,
    revision: &str,
    label: &str,
    path: &str,
) -> Result<PilotNoteInstruction, String> {
    let bytes = read_blob_at(repo_root, revision, path)?;
    PilotNoteInstruction::admit(label, path, revision, &bytes)
}
