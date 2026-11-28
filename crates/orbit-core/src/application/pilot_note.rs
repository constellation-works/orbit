//! Pilot notes: reading committed instruction files mapped under
//! `[workflow.pilot_notes.<label>]` for `task_pilot` dispatches and `orbit
//! doctor` checks.
//!
//! Files are always read from a pinned Git commit (never from a worktree).
//! An unusable file switches off only its own label for that run and records
//! the reason in audit rather than refusing the pilot.

use std::path::Path;

use orbit_config::PilotNoteSettings;
use orbit_types::workflow::PilotNoteInstruction;

use crate::application::task::base_revision::{commit_at, read_blob_at};

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

/// The notes `settings` map, admitted at `revision`, in label order.
///
/// A revision that does not resolve, or a file that is unusable, leaves its
/// label out; the caller sees only the instructions a pilot could have been
/// given. Audit of the refusals belongs to the dispatch that rendered them.
pub(crate) fn admitted_pilot_notes_at(
    repo_root: &Path,
    settings: &PilotNoteSettings,
    revision: &str,
) -> Vec<PilotNoteInstruction> {
    let Ok(commit) = commit_at(repo_root, revision) else {
        return Vec::new();
    };
    settings
        .entries()
        .filter_map(|(label, path)| resolve_pilot_note_at(repo_root, &commit, label, path).ok())
        .collect()
}
