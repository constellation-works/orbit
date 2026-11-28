//! Pilot notes: reading committed instruction files mapped under
//! `[workflow.pilot_notes.<label>]` for `task_pilot` dispatches and `orbit
//! doctor` checks.
//!
//! Files are always read from a pinned Git commit (never from a worktree).
//! An unusable file switches off only its own label for that run and records
//! the reason in audit rather than refusing the pilot.

use std::path::Path;

use orbit_types::workflow::PilotNoteInstruction;

use crate::application::task::base_revision::read_blob_at;

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
