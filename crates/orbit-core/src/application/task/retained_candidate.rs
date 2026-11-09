//! The failed run's retained candidate: the worktree a failed run leaves
//! behind, where its unfinished work stays after the run ends. The
//! blocked-task backstop names it to its agent and in every escalation; it
//! only ever reads it.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration as StdDuration;

use orbit_common::process::output_capture::OUTPUT_TRUNCATED_MARKER;
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

// A test cap smaller than a fixture's porcelain, so truncation does not
// need a worktree large enough to fill 256 KiB.
#[cfg(test)]
thread_local! {
    pub(super) static CANDIDATE_STATUS_LIMIT_OVERRIDE: std::cell::Cell<Option<usize>> =
        const { std::cell::Cell::new(None) };
}

fn candidate_status_limit() -> usize {
    #[cfg(test)]
    if let Some(limit) = CANDIDATE_STATUS_LIMIT_OVERRIDE.with(|limit| limit.get()) {
        return limit;
    }
    CANDIDATE_STATUS_LIMIT
}

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
        /// Changed paths beyond the listed ones. A lower bound when
        /// `changed_paths_truncated` is set: the status listing was cut
        /// before every path was read.
        omitted_paths: usize,
        /// `git status` output exceeded the capture limit, so `omitted_paths`
        /// counts only the retained prefix.
        changed_paths_truncated: bool,
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
                changed_paths_truncated,
            } => {
                let mut value = json!({
                    "status": "present",
                    "path": path.to_string_lossy(),
                    "changed_paths_omitted": omitted_paths,
                });
                if *changed_paths_truncated {
                    value["changed_paths_truncated"] = json!(true);
                }
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
                changed_paths_truncated,
            } => {
                let changes = match changed_paths {
                    Ok(paths) if paths.is_empty() && *changed_paths_truncated => {
                        "changed paths truncated before a complete record".to_string()
                    }
                    Ok(paths) if paths.is_empty() => "no uncommitted changes".to_string(),
                    Ok(paths) if *changed_paths_truncated && *omitted_paths > 0 => format!(
                        "changed: {} (+{omitted_paths} more, listing truncated)",
                        paths.join(", ")
                    ),
                    Ok(paths) if *changed_paths_truncated => {
                        format!("changed: {} (listing truncated)", paths.join(", "))
                    }
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
        let mut changed_paths_truncated = false;
        let changed_paths = changed_paths(&path).map(|listing| {
            changed_paths_truncated = listing.truncated;
            omitted_paths = listing.paths.len().saturating_sub(MAX_CANDIDATE_PATHS);
            let mut paths = listing.paths;
            paths.truncate(MAX_CANDIDATE_PATHS);
            paths
        });
        RetainedCandidate::Present {
            path,
            changed_paths,
            omitted_paths,
            changed_paths_truncated,
        }
    }
}

/// Paths `git status` kept, and whether the capture stopped early.
struct CandidatePathListing {
    paths: Vec<String>,
    truncated: bool,
}

/// Paths `git status` reports in a worktree — staged, unstaged or untracked —
/// without writing to it: optional locks are off, so Git neither takes the
/// index lock nor refreshes the index, and no fsmonitor program runs. Status
/// never recurses into a gitlink's checkout, whose own config could run a
/// filter command on the host.
fn changed_paths(worktree: &Path) -> Result<CandidatePathListing, String> {
    let mut command = Command::new("git");
    command
        .current_dir(worktree)
        .args([
            "-c",
            "core.fsmonitor=false",
            "-c",
            "diff.ignoreSubmodules=all",
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
        candidate_status_limit(),
    )
    .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "git status exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let truncated = output.stdout.ends_with(OUTPUT_TRUNCATED_MARKER);
    let retained = if truncated {
        &output.stdout[..output.stdout.len() - OUTPUT_TRUNCATED_MARKER.len()]
    } else {
        &output.stdout
    };
    // A cut that lands inside a record leaves a prefix that is not a path.
    let partial_tail = truncated && !retained.is_empty() && !retained.ends_with(b"\0");
    let stdout = String::from_utf8_lossy(retained);
    let mut segments: Vec<&str> = stdout
        .split('\0')
        .filter(|record| !record.is_empty())
        .collect();
    if partial_tail {
        segments.pop();
    }
    let mut records = segments.into_iter();
    let mut paths = Vec::new();
    while let Some(record) = records.next() {
        let (Some(status), Some(path)) = (record.get(..2), record.get(3..)) else {
            continue;
        };
        // A rename or copy is followed by the path it came from. A truncated
        // listing that lost that path cannot name the rename.
        if status.contains(['R', 'C']) && records.next().is_none() && truncated {
            break;
        }
        paths.push(path.to_string());
    }
    Ok(CandidatePathListing { paths, truncated })
}
