//! The failed run's retained candidate: the worktree a failed run leaves
//! behind, where its unfinished work stays after the run ends. The
//! blocked-task backstop names it to its agent and in every escalation; it
//! only ever reads it.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration as StdDuration;

use orbit_common::process::run_bounded_capped;
use orbit_engine::run_worktree_paths;
use orbit_types::task::Task;
use orbit_types::workflow::JobRun;
use serde_json::{Value, json};

use crate::OrbitRuntime;

/// Changed paths of a retained candidate listed in the agent input and in an
/// escalation; the rest are counted.
const MAX_CANDIDATE_PATHS: usize = 20;
/// Bounds on reading a retained candidate's status.
const CANDIDATE_STATUS_TIMEOUT: StdDuration = StdDuration::from_secs(30);
const CANDIDATE_STATUS_LIMIT: usize = 256 * 1024;

/// The failed run's own worktree, where its unfinished candidate stays after
/// the run ends. The backstop reads it; it never writes or copies it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RetainedCandidate {
    /// The worktree still exists on this machine.
    Present {
        /// The worktree directory.
        path: PathBuf,
        /// Changed and untracked paths relative to the worktree, at most
        /// [`MAX_CANDIDATE_PATHS`]; `Err` when Git could not list them.
        changed_paths: Result<Vec<String>, String>,
        /// Changed paths beyond the listed ones.
        omitted_paths: usize,
    },
    /// No worktree to point at, and why.
    Absent {
        /// Why there is none.
        reason: String,
    },
}

impl RetainedCandidate {
    /// The agent-input form.
    pub(crate) fn to_json(&self) -> Value {
        match self {
            Self::Present {
                path,
                changed_paths,
                omitted_paths,
            } => {
                let mut value = json!({
                    "status": "present",
                    "path": path.to_string_lossy(),
                    "changed_paths_omitted": omitted_paths,
                });
                match changed_paths {
                    Ok(paths) => value["changed_paths"] = json!(paths),
                    Err(error) => value["changed_paths_error"] = json!(error),
                }
                value
            }
            Self::Absent { reason } => json!({"status": "absent", "reason": reason}),
        }
    }

    /// One line naming the candidate, for an escalation.
    pub(crate) fn summary(&self) -> String {
        match self {
            Self::Present {
                path,
                changed_paths,
                omitted_paths,
            } => {
                let changes = match changed_paths {
                    Ok(paths) if paths.is_empty() => "no uncommitted changes".to_string(),
                    Ok(paths) if *omitted_paths > 0 => {
                        format!("changed: {} (+{omitted_paths} more)", paths.join(", "))
                    }
                    Ok(paths) => format!("changed: {}", paths.join(", ")),
                    Err(error) => format!("changes unreadable: {error}"),
                };
                format!(
                    "retained candidate: {} ({changes}); it is left untouched",
                    path.display()
                )
            }
            Self::Absent { reason } => format!("retained candidate: absent ({reason})"),
        }
    }
}

impl OrbitRuntime {
    /// Locate the failed run's worktree by the rule its setup used and list
    /// what it changed, without writing to it.
    pub(crate) fn retained_candidate(
        &self,
        task: &Task,
        failed_run_id: Option<&str>,
        failed_run: Option<&JobRun>,
    ) -> RetainedCandidate {
        let absent = |reason: String| RetainedCandidate::Absent { reason };
        let Some(run_id) = failed_run_id else {
            return absent("no failed run is recorded for this block".to_string());
        };
        let Some(run) = failed_run else {
            return absent(match &task.job_run_machine {
                Some(location) if !self.task_run_is_local(task) => format!(
                    "run {run_id} executed on {} ({}); its worktree is on that machine",
                    location
                        .machine_name
                        .as_deref()
                        .unwrap_or(&location.machine_id),
                    location.machine_id,
                ),
                _ => format!("run {run_id} has no record on this machine"),
            });
        };
        let repo_root = &self.paths().repo_root;
        let paths = match run_worktree_paths(repo_root, run) {
            Ok(paths) => paths,
            Err(error) => return absent(format!("run {run_id} names no worktree: {error}")),
        };
        let Some(path) = paths.into_iter().find(|path| path.is_dir()) else {
            return absent(format!("run {run_id}'s worktree no longer exists"));
        };
        let mut omitted_paths = 0;
        let changed_paths = changed_paths(&path).map(|mut paths| {
            omitted_paths = paths.len().saturating_sub(MAX_CANDIDATE_PATHS);
            paths.truncate(MAX_CANDIDATE_PATHS);
            paths
        });
        RetainedCandidate::Present {
            path,
            changed_paths,
            omitted_paths,
        }
    }
}

/// Paths `git status` reports in a worktree — staged, unstaged or untracked —
/// without writing to it: optional locks are off, so Git neither takes the
/// index lock nor refreshes the index, and no fsmonitor program runs.
fn changed_paths(worktree: &Path) -> Result<Vec<String>, String> {
    let mut command = Command::new("git");
    command
        .current_dir(worktree)
        .args([
            "-c",
            "core.fsmonitor=false",
            "--no-optional-locks",
            "status",
            "--porcelain=v1",
            "-z",
        ])
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0");
    let output = run_bounded_capped(
        &mut command,
        CANDIDATE_STATUS_TIMEOUT,
        CANDIDATE_STATUS_LIMIT,
    )
    .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "git status exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut records = stdout.split('\0').filter(|record| !record.is_empty());
    let mut paths = Vec::new();
    while let Some(record) = records.next() {
        let (Some(status), Some(path)) = (record.get(..2), record.get(3..)) else {
            continue;
        };
        // A rename or copy is followed by the path it came from.
        if status.contains(['R', 'C']) {
            records.next();
        }
        paths.push(path.to_string());
    }
    Ok(paths)
}
