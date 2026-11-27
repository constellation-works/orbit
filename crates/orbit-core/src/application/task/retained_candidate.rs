//! The failed run's retained candidate: the worktree a failed run leaves
//! behind, where its unfinished work stays after the run ends. The
//! blocked-task backstop names it to its agent and in every escalation; it
//! only ever reads it.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration as StdDuration;

use orbit_common::process::output_capture::OUTPUT_TRUNCATED_MARKER;
use orbit_common::process::run_bounded_capped;
use orbit_engine::review_gate::RESCUE_CANDIDATE_KEY;
use orbit_engine::run_worktree_paths;
use orbit_types::task::Task;
use orbit_types::workflow::JobRun;
use serde_json::{Value, json};

use super::TaskJobPort;
use crate::OrbitRuntime;

/// Changed paths of a retained candidate listed in the agent input and in an
/// escalation; the rest are counted.
const MAX_CANDIDATE_PATHS: usize = 20;
/// Bounds on reading a retained candidate's status.
const CANDIDATE_STATUS_TIMEOUT: StdDuration = StdDuration::from_secs(30);
const CANDIDATE_STATUS_LIMIT: usize = 256 * 1024;

/// Largest retained final-recovery patch an operator can adopt.
const RETAINED_PATCH_LIMIT: usize = 8 * 1024 * 1024;

// A test cap smaller than a fixture's porcelain, so truncation does not
// need a worktree large enough to fill 256 KiB.
#[cfg(test)]
thread_local! {
    pub(super) static CANDIDATE_STATUS_LIMIT_OVERRIDE: std::cell::Cell<Option<usize>> =
        const { std::cell::Cell::new(None) };
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
        let paths = match run_worktree_paths(self.env(), repo_root, run) {
            Ok(paths) => paths,
            Err(error) => return absent(format!("run {run_id} names no worktree: {error}")),
        };
        let Some(path) = paths.into_iter().find(|path| path.is_dir()) else {
            let rescue = self
                .stored_run_state(run_id)
                .ok()
                .flatten()
                .and_then(|state| state.pipeline.get(RESCUE_CANDIDATE_KEY).cloned())
                .filter(|record| record["run_id"].as_str() == Some(run_id));
            return absent(match rescue {
                Some(record) => match (
                    record["rescue_ref"].as_str(),
                    record["rescue_commit"].as_str(),
                ) {
                    (Some(reference), Some(commit)) => format!(
                        "run {run_id}'s worktree no longer exists; rescue candidate kept at \
                         {reference} (commit {commit})"
                    ),
                    _ => format!("run {run_id}'s worktree no longer exists"),
                },
                None => format!("run {run_id}'s worktree no longer exists"),
            });
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

/// The exact uncommitted tracked edits of a stopped run's retained checkout,
/// read without writing to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RetainedPatch {
    /// The checkout's `HEAD`, which the patch applies to.
    pub(crate) head: String,
    /// `git diff --binary --full-index HEAD` output.
    pub(crate) bytes: Vec<u8>,
    /// Changed tracked paths, in `git status` order.
    pub(crate) paths: Vec<String>,
}

impl OrbitRuntime {
    /// Read the retained checkout's uncommitted tracked edits for explicit
    /// adoption. `Ok(None)` is a clean checkout. Refuses (`Err`) a missing
    /// checkout, untracked or unmerged paths, an incomplete listing and a
    /// patch beyond the adoption limit: none of those is an exact patch.
    pub(crate) fn retained_patch(&self, run: &JobRun) -> Result<Option<RetainedPatch>, String> {
        let paths = run_worktree_paths(self.env(), &self.paths().repo_root, run)
            .map_err(|error| format!("run {} names no worktree: {error}", run.run_id))?;
        let Some(worktree) = paths.into_iter().find(|path| path.is_dir()) else {
            return Err(format!("run {}'s worktree no longer exists", run.run_id));
        };
        let (status, truncated) = read_only_git(
            &worktree,
            &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
            candidate_status_limit(),
        )?;
        if truncated {
            return Err("the retained checkout's status listing is truncated".into());
        }
        let status = String::from_utf8(status)
            .map_err(|_| "the retained checkout's status names a non-UTF-8 path".to_string())?;
        let mut records = status.split('\0').filter(|record| !record.is_empty());
        let mut changed = Vec::new();
        while let Some(record) = records.next() {
            let (Some(code), Some(path)) = (record.get(..2), record.get(3..)) else {
                return Err("the retained checkout's status is unreadable".into());
            };
            if code == "??" {
                return Err(format!("the retained checkout has untracked path {path}"));
            }
            if code.contains('U') || code == "AA" || code == "DD" {
                return Err(format!("the retained checkout has unmerged path {path}"));
            }
            if code.contains(['R', 'C']) {
                records.next();
            }
            changed.push(path.to_string());
        }
        if changed.is_empty() {
            return Ok(None);
        }
        let (head, _) = read_only_git(&worktree, &["rev-parse", "--verify", "HEAD"], 256)?;
        let head = String::from_utf8_lossy(&head).trim().to_string();
        let (bytes, truncated) = read_only_git(
            &worktree,
            &[
                "diff",
                "--binary",
                "--full-index",
                "--no-ext-diff",
                "--no-textconv",
                "--no-color",
                "--no-renames",
                "HEAD",
                "--",
            ],
            RETAINED_PATCH_LIMIT,
        )?;
        if truncated {
            return Err(format!(
                "the retained edits exceed the {RETAINED_PATCH_LIMIT}-byte adoption limit"
            ));
        }
        // The patch is read with submodule inspection off, so it can omit a
        // gitlink that status still lists. A patch that skips a listed path
        // would carry that path only by name; refuse instead.
        let (diffed, diffed_truncated) = read_only_git(
            &worktree,
            &["diff", "--name-only", "-z", "--no-renames", "HEAD", "--"],
            candidate_status_limit(),
        )?;
        if diffed_truncated {
            return Err("the retained checkout's changed-path listing is truncated".into());
        }
        let diffed: BTreeSet<&[u8]> = diffed
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
            .collect();
        if let Some(omitted) = changed
            .iter()
            .find(|path| !diffed.contains(path.as_bytes()))
        {
            return Err(format!(
                "the retained edit to {omitted} is a Git link (submodule) change or is otherwise absent from the patch, so it cannot be adopted exactly; commit or revert it in the retained checkout and inspect again"
            ));
        }
        if bytes.is_empty() {
            return Err("the retained checkout's status and diff disagree".into());
        }
        Ok(Some(RetainedPatch {
            head,
            bytes,
            paths: changed,
        }))
    }
}

/// Paths `git status` kept, and whether the capture stopped early.
struct CandidatePathListing {
    paths: Vec<String>,
    truncated: bool,
}

fn candidate_status_limit() -> usize {
    #[cfg(test)]
    if let Some(limit) = CANDIDATE_STATUS_LIMIT_OVERRIDE.with(|limit| limit.get()) {
        return limit;
    }
    CANDIDATE_STATUS_LIMIT
}

/// Run Git in `worktree` without writing to it: optional locks are off, so
/// Git neither takes the index lock nor refreshes the index, and no fsmonitor
/// program runs. Nothing recurses into a gitlink's checkout, whose own config
/// could run a filter command on the host. Returns stdout without the
/// truncation marker, and whether the capture stopped at `limit`.
fn read_only_git(worktree: &Path, args: &[&str], limit: usize) -> Result<(Vec<u8>, bool), String> {
    let mut command = Command::new("git");
    command
        .current_dir(worktree)
        .args([
            "-c",
            "core.fsmonitor=false",
            "-c",
            "diff.ignoreSubmodules=all",
            "--no-optional-locks",
        ])
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0");
    let mut output = run_bounded_capped(&mut command, CANDIDATE_STATUS_TIMEOUT, limit)
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "git {} exited with {}: {}",
            args.first().copied().unwrap_or_default(),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let truncated = output.stdout.ends_with(OUTPUT_TRUNCATED_MARKER);
    if truncated {
        output
            .stdout
            .truncate(output.stdout.len() - OUTPUT_TRUNCATED_MARKER.len());
    }
    Ok((output.stdout, truncated))
}

/// Paths `git status` reports in a worktree — staged, unstaged or untracked —
/// without writing to it: optional locks are off, so Git neither takes the
/// index lock nor refreshes the index, and no fsmonitor program runs. Status
/// never recurses into a gitlink's checkout, whose own config could run a
/// filter command on the host.
fn changed_paths(worktree: &Path) -> Result<CandidatePathListing, String> {
    let (stdout, truncated) = read_only_git(
        worktree,
        &["status", "--porcelain=v1", "-z"],
        candidate_status_limit(),
    )?;
    let retained = stdout.as_slice();
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
