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
/// reached through more than one name is written once. Links outside the
/// workspace are rejected before either guide is changed, and every target is
/// planned before any is written: a marker problem in one guide leaves all of
/// them byte-identical.
pub fn inject_agent_rules(workspace_root: &Path) -> Result<InjectAgentRulesResult, OrbitError> {
    let block = normalized_block(AGENT_RULES_TEMPLATE)?;
    let root = std::fs::canonicalize(workspace_root)
        .map_err(|e| OrbitError::Io(format!("resolve {}: {e}", workspace_root.display())))?;
    let mut targets = Vec::with_capacity(TARGET_FILES.len());
    for name in TARGET_FILES {
        let guide = root.join(name);
        let path = match std::fs::symlink_metadata(&guide) {
            Ok(_) => std::fs::canonicalize(&guide)
                .map_err(|e| OrbitError::Io(format!("resolve {}: {e}", guide.display())))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => guide.clone(),
            Err(e) => return Err(OrbitError::Io(format!("inspect {}: {e}", guide.display()))),
        };
        if !path.starts_with(&root) {
            return Err(OrbitError::InvalidInput(format!(
                "{} resolves outside workspace {} to {} — refusing to inject agent rules",
                guide.display(),
                root.display(),
                path.display()
            )));
        }
        if !targets.contains(&path) {
            targets.push(path);
        }
    }

    let plans = targets
        .into_iter()
        .map(|path| plan_file(&path, &block))
        .collect::<Result<Vec<_>, _>>()?;

    let mut outcomes: Vec<InjectionOutcome> = Vec::with_capacity(plans.len());
    for plan in plans {
        if let Some(next) = &plan.next {
            atomic_write_text(&plan.path, next).map_err(|e| OrbitError::Io(e.to_string()))?;
        }
        outcomes.push(InjectionOutcome {
            path: plan.path,
            action: plan.action,
        });
    }
    Ok(InjectAgentRulesResult { outcomes })
}

/// A target's outcome, with the content it should hold once written.
struct PlannedWrite {
    path: PathBuf,
    action: InjectionAction,
    /// `None` when the file already holds the block and needs no write.
    next: Option<String>,
}

/// Normalize the template to a single trailing newline so the file produced
/// from a brand-new write ends cleanly.
fn normalized_block(template: &str) -> Result<String, OrbitError> {
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

/// Compute what `path` should hold after injection, without writing it. Every
/// refusal fires here, so the caller writes nothing unless all targets pass.
fn plan_file(path: &Path, block: &str) -> Result<PlannedWrite, OrbitError> {
    if !path.exists() {
        return Ok(PlannedWrite {
            path: path.to_path_buf(),
            action: InjectionAction::Created,
            next: Some(block.to_string()),
        });
    }
    let existing = std::fs::read_to_string(path)
        .map_err(|e| OrbitError::Io(format!("read {}: {e}", path.display())))?;
    let has_start = existing.contains(START_MARKER);
    let has_end = existing.contains(END_MARKER);
    match (has_start, has_end) {
        (false, false) => {
            let mut next = existing;
            if !next.ends_with('\n') {
                next.push('\n');
            }
            // One blank-line separator between prior content and the block.
            next.push('\n');
            next.push_str(block);
            Ok(PlannedWrite {
                path: path.to_path_buf(),
                action: InjectionAction::AppendedBlock,
                next: Some(next),
            })
        }
        (true, true) => {
            let next = splice_block(&existing, block, path)?;
            // No-op when the block already byte-matches; leaving `next` unset
            // keeps the file's mtime unchanged.
            let changed = next != existing;
            Ok(PlannedWrite {
                path: path.to_path_buf(),
                action: InjectionAction::ReplacedBlock,
                next: changed.then_some(next),
            })
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
