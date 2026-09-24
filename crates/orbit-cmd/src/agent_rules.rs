//! Opt-in injection of Orbit's workflow rules into agent-prompt files
//! (`CLAUDE.md`, `AGENTS.md`) at the workspace root.
//!
//! Triggered by `orbit workspace init --inject-agent-rules`. The rule content
//! lives in `crates/orbit-cmd/assets/agent-rules.md` as a self-contained
//! fenced block (with start/end markers literally inside the asset). Re-runs
//! replace only the content between the markers; content outside is
//! byte-preserved.

use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::fs::io::atomic_write_text;

/// Asset block embedded at compile time. The asset contains the marker pair
/// literally so a read-render-write round-trip is byte-stable when the asset
/// has not changed.
pub const AGENT_RULES_TEMPLATE: &str = include_str!("../assets/agent-rules.md");

pub const START_MARKER: &str = "<!-- orbit-managed:start -->";
pub const END_MARKER: &str = "<!-- orbit-managed:end -->";

const TARGET_FILES: &[&str] = &["CLAUDE.md", "AGENTS.md"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InjectionAction {
    Created,
    AppendedBlock,
    ReplacedBlock,
}

#[derive(Debug, Clone)]
pub struct InjectionOutcome {
    pub path: PathBuf,
    pub action: InjectionAction,
}

#[derive(Debug, Clone)]
pub struct InjectAgentRulesResult {
    pub outcomes: Vec<InjectionOutcome>,
}

/// Inject (or refresh) the Orbit rules block into `CLAUDE.md` and `AGENTS.md`
/// at the workspace root. Always uses the embedded `AGENT_RULES_TEMPLATE`.
///
/// A target that is a symlink (commonly `CLAUDE.md -> AGENTS.md`) is written
/// through to the file it names: the atomic rename would otherwise replace
/// the link with a copy, and both names would then drift apart. A file
/// reached through more than one name is written once.
pub fn inject_agent_rules(workspace_root: &Path) -> Result<InjectAgentRulesResult, OrbitError> {
    let block = normalized_block(AGENT_RULES_TEMPLATE)?;
    let mut outcomes: Vec<InjectionOutcome> = Vec::with_capacity(TARGET_FILES.len());
    for name in TARGET_FILES {
        let name = workspace_root.join(name);
        let path = std::fs::canonicalize(&name).unwrap_or(name);
        if outcomes.iter().any(|outcome| outcome.path == path) {
            continue;
        }
        let action = apply_to_file(&path, &block)?;
        outcomes.push(InjectionOutcome { path, action });
    }
    Ok(InjectAgentRulesResult { outcomes })
}

/// Normalize the template to a single trailing newline so the file produced
/// from a brand-new write ends cleanly.
// Widened to pub(crate) so sibling `src/tests/agent_rules.rs` can pin the
// trim/marker shape after the test-layout migration.
pub(crate) fn normalized_block(template: &str) -> Result<String, OrbitError> {
    if !template.contains(START_MARKER) || !template.contains(END_MARKER) {
        return Err(OrbitError::InvalidInput(format!(
            "agent-rules template missing required markers ({START_MARKER} / {END_MARKER})"
        )));
    }
    let trimmed = template.trim_end_matches('\n');
    let mut block = String::with_capacity(trimmed.len() + 1);
    block.push_str(trimmed);
    block.push('\n');
    Ok(block)
}

// Widened to pub(crate) so sibling `src/tests/agent_rules.rs` can exercise
// per-file apply/replace/reject paths after the test-layout migration.
pub(crate) fn apply_to_file(path: &Path, block: &str) -> Result<InjectionAction, OrbitError> {
    if !path.exists() {
        atomic_write_text(path, block).map_err(|e| OrbitError::Io(e.to_string()))?;
        return Ok(InjectionAction::Created);
    }
    let existing = std::fs::read_to_string(path)
        .map_err(|e| OrbitError::Io(format!("read {}: {e}", path.display())))?;
    let has_start = existing.contains(START_MARKER);
    let has_end = existing.contains(END_MARKER);
    match (has_start, has_end) {
        (false, false) => {
            let mut next = existing.clone();
            if !next.ends_with('\n') {
                next.push('\n');
            }
            // One blank-line separator between prior content and the block.
            next.push('\n');
            next.push_str(block);
            atomic_write_text(path, &next).map_err(|e| OrbitError::Io(e.to_string()))?;
            Ok(InjectionAction::AppendedBlock)
        }
        (true, true) => {
            let next = splice_block(&existing, block, path)?;
            if next == existing {
                // No-op — block already byte-matches; skip the write so file
                // mtime does not change unnecessarily.
                return Ok(InjectionAction::ReplacedBlock);
            }
            atomic_write_text(path, &next).map_err(|e| OrbitError::Io(e.to_string()))?;
            Ok(InjectionAction::ReplacedBlock)
        }
        (true, false) => Err(OrbitError::InvalidInput(format!(
            "{}: contains `{START_MARKER}` without matching `{END_MARKER}` — refusing to write; resolve manually",
            path.display()
        ))),
        (false, true) => Err(OrbitError::InvalidInput(format!(
            "{}: contains `{END_MARKER}` without matching `{START_MARKER}` — refusing to write; resolve manually",
            path.display()
        ))),
    }
}

/// Replace the first marker-bounded span in `existing` with `block`. Caller
/// has already verified both markers are present.
fn splice_block(existing: &str, block: &str, path: &Path) -> Result<String, OrbitError> {
    let start = existing.find(START_MARKER).ok_or_else(|| {
        OrbitError::InvalidInput(format!(
            "{}: start marker disappeared between checks",
            path.display()
        ))
    })?;
    let end_idx = existing
        .match_indices(END_MARKER)
        .find(|(idx, _)| *idx > start)
        .map(|(idx, _)| idx + END_MARKER.len())
        .ok_or_else(|| {
            OrbitError::InvalidInput(format!(
                "{}: end marker appears before start marker — refusing to write; resolve manually",
                path.display()
            ))
        })?;

    let mut next = String::with_capacity(existing.len() + block.len());
    next.push_str(&existing[..start]);
    let trimmed_block = block.trim_end_matches('\n');
    next.push_str(trimmed_block);
    next.push_str(&existing[end_idx..]);
    if !next.ends_with('\n') {
        next.push('\n');
    }
    Ok(next)
}
