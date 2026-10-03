//! ORB-10603: a deterministic execution summary derived from the delivered change.
//!
//! Delivery refuses to hand off a task whose durable execution summary is empty
//! (see [`reject_failed_delivery`](super::super::handoff::reject_failed_delivery)),
//! and the only writer of that field was a prose instruction asking the
//! implementing agent to persist one. Agent-loop output is advisory, so every
//! run where the agent skipped the instruction wedged at commit.
//!
//! This module closes the gap without touching either end of the contract. It
//! never reads the agent's returned envelope — the summary is derived from the
//! worktree change this step is about to deliver, which is durable state any
//! reviewer can re-check with `git status` before the commit or `git show
//! --stat` after it. It also never overwrites a summary the agent did persist:
//! an agent-authored account of the work is the better artifact and wins.
//!
//! ADR-0326 records the decision and the alternatives it rejected.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_types::task::Task;

use crate::context::{RuntimeHost, TaskAutomationUpdate};

use super::super::git::git_output_raw;
use super::super::pr::meaningful_execution_summary;

/// Task event recorded when Orbit, not an agent, authored the summary.
const DERIVED_SUMMARY_EVENT: &str = "execution_summary_derived";

/// Cap on individually named files; the remainder is reported as a count.
const MAX_LISTED_FILES: usize = 25;

/// Give the task a durable execution summary when nothing else did.
///
/// Returns the task the caller must use from here on: unchanged when a
/// meaningful summary is already persisted or when the worktree carries no
/// change to describe, otherwise carrying the derived summary that was just
/// written to the task record.
pub(super) fn ensure_durable_execution_summary<H: RuntimeHost + ?Sized>(
    host: &H,
    task: Task,
    workspace_path: &Path,
    run_id: &str,
) -> Result<Task, OrbitError> {
    if meaningful_execution_summary(&task.execution_summary).is_some() {
        return Ok(task);
    }

    let changes = worktree_changes(workspace_path)?;
    if changes.is_empty() {
        // Nothing deterministic to say. The delivery gate still refuses the
        // empty summary, and the commit step still reports the empty stage.
        return Ok(task);
    }

    let summary = render_derived_summary(&task.id, run_id, &changes);
    host.apply_task_automation_update(
        &task.id,
        TaskAutomationUpdate {
            execution_summary: Some(summary.clone()),
            status_event: Some(DERIVED_SUMMARY_EVENT.to_string()),
            status_note: Some(format!(
                "automation: derived execution_summary from {} changed file(s) in the delivery \
                 worktree",
                changes.len()
            )),
            ..TaskAutomationUpdate::default()
        },
    )?;

    let mut task = task;
    task.execution_summary = summary;
    Ok(task)
}

/// One worktree entry, reduced to the two facts a summary reports.
#[derive(Debug, Eq, PartialEq)]
struct WorktreeChange {
    pub(super) kind: &'static str,
    pub(super) path: String,
}

/// Read the change this step will deliver without staging or mutating anything.
///
/// `--untracked-files=all` matches what `git add --all` would stage, so the
/// derived summary describes the same file set the commit contains.
fn worktree_changes(workspace_path: &Path) -> Result<Vec<WorktreeChange>, OrbitError> {
    let raw = git_output_raw(
        workspace_path,
        &["status", "--porcelain=v1", "--untracked-files=all", "-z"],
    )?;
    Ok(parse_status_entries(&raw))
}

/// Parse `git status --porcelain=v1 -z` output.
///
/// Each record is `XY <path>` terminated by NUL; a rename or copy is followed
/// by a second NUL-terminated field holding the source path, which belongs to
/// the record before it rather than starting a new one.
fn parse_status_entries(raw: &str) -> Vec<WorktreeChange> {
    let mut changes = Vec::new();
    let mut fields = raw.split('\0');
    while let Some(entry) = fields.next() {
        if entry.is_empty() {
            continue;
        }
        let mut codes = entry.chars();
        let (Some(index_state), Some(worktree_state)) = (codes.next(), codes.next()) else {
            continue;
        };
        // `XY ` is always three ASCII bytes, so byte 3 is a char boundary.
        let path = entry.get(3..).unwrap_or_default().trim();
        if matches!(index_state, 'R' | 'C') {
            // Consume the rename/copy source field so it is not read as a record.
            let _ = fields.next();
        }
        if path.is_empty() {
            continue;
        }
        changes.push(WorktreeChange {
            kind: change_kind(index_state, worktree_state),
            path: path.to_string(),
        });
    }
    changes.sort_by(|left, right| left.path.cmp(&right.path));
    changes
}

fn change_kind(index_state: char, worktree_state: char) -> &'static str {
    if index_state == '?' {
        return "added";
    }
    if index_state == 'U'
        || worktree_state == 'U'
        || (index_state == 'A' && worktree_state == 'A')
        || (index_state == 'D' && worktree_state == 'D')
    {
        return "unmerged";
    }
    let state = if index_state == ' ' {
        worktree_state
    } else {
        index_state
    };
    match state {
        'A' => "added",
        'M' => "modified",
        'D' => "deleted",
        'R' => "renamed",
        'C' => "copied",
        'T' => "type-changed",
        _ => "changed",
    }
}

fn render_derived_summary(task_id: &str, run_id: &str, changes: &[WorktreeChange]) -> String {
    let mut lines = vec![
        format!(
            "Execution summary derived by Orbit for {task_id} from the change delivered by run \
             {run_id}; no execution summary was persisted by the implementing agent."
        ),
        String::new(),
        format!("Changed files ({}):", changes.len()),
    ];
    for change in changes.iter().take(MAX_LISTED_FILES) {
        lines.push(format!("- {}: {}", change.kind, change.path));
    }
    let remaining = changes.len().saturating_sub(MAX_LISTED_FILES);
    if remaining > 0 {
        lines.push(format!("- ... and {remaining} more file(s)"));
    }
    lines.push(String::new());
    lines.push(
        "This restates the worktree change the delivery commit contains and asserts nothing \
         beyond it. Re-check it with `git show --stat` on the delivery commit."
            .to_string(),
    );
    lines.join("\n")
}
