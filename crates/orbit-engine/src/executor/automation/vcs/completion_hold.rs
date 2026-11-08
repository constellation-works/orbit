//! `completion.hold` [ORB-23322]: the rules that lower a completion-authorized
//! run's `done` to `review`.
//!
//! Completion stays an explicit per-invocation grant
//! ([`orbit_types::workflow::CompletionPolicy`]); these rules can only hold it
//! back. `pr_complete` evaluates them against the files the pull request
//! changes before it merges anything, and final recovery against the task
//! alone before it settles a `complete_no_diff` as `done`. Local delivery
//! evaluates them in `git_merge` before the candidate lands on the base, and
//! an owner's landing of a claimed leaf under
//! `workflow.distributed_completion = "done"` evaluates them before it
//! merges or fast-forwards anything [ORB-23365].

use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::fs::path_filter::PathFilter;
use orbit_types::task::{Task, TaskComment, TaskHistoryEntry};
use orbit_types::workflow::{
    COMPLETION_HELD_EVENT, CompletionHold, CompletionHoldField, CompletionHoldRule,
    completion_hold_complexity_rank, current_completion_hold, normalize_instruction_tag,
};

use crate::context::{RuntimeHost, TaskAutomationUpdate};

use super::git::{git_command_success, git_output_paths};

/// What a hold's `paths` rules are matched against.
pub(crate) enum ChangedPaths {
    /// The files the delivery changes.
    Read(Vec<String>),
    /// The changed files could not be read; a `paths` rule holds on it,
    /// because an unread diff cannot prove a risky path untouched.
    Unreadable(String),
    /// The delivery carries no diff of its own (a no-diff or already-landed
    /// completion); `paths` rules have nothing to match.
    NoDiff,
}

/// A completion-authorized run's hold policy, carried into the merge
/// driver so the candidate that can actually reach a merge request is
/// evaluated, not only the originally published head [ORB-23445].
#[derive(Clone)]
pub(crate) struct CompletionHoldCheck {
    pub(crate) rules: Vec<CompletionHoldRule>,
    pub(crate) tasks: Vec<Task>,
    pub(crate) run_id: String,
}

impl CompletionHoldCheck {
    /// Evaluate the rules against the diff of `candidate` into the input's
    /// base. `None` candidate fails closed through [`ChangedPaths::Unreadable`]
    /// when a `paths` rule is configured, mirroring the pre-drive check.
    pub(crate) fn evaluate(
        &self,
        workspace: &Path,
        base: Option<&str>,
        base_sync_local: bool,
        candidate: Option<&str>,
    ) -> Result<Option<CompletionHold>, OrbitError> {
        let changed = pull_request_changed_paths(workspace, base, base_sync_local, candidate);
        evaluate_completion_hold(&self.rules, &self.tasks, &changed, &self.run_id)
    }
}

/// The first rule, in configured order, that holds any task of the bundle:
/// within a rule, `paths`, then `complexity_at_least`, `types` and `tags`.
pub(crate) fn evaluate_completion_hold(
    rules: &[CompletionHoldRule],
    tasks: &[Task],
    changed: &ChangedPaths,
    run_id: &str,
) -> Result<Option<CompletionHold>, OrbitError> {
    for (index, rule) in rules.iter().enumerate() {
        let label = rule.label(index);
        let hold = |field, pattern: String, matched: String, task: Option<&Task>| CompletionHold {
            rule: label.clone(),
            field,
            pattern,
            matched,
            matched_task_id: task.map(|task| task.id.clone()),
            run_id: run_id.to_string(),
        };
        if !rule.paths.is_empty() {
            match changed {
                ChangedPaths::Read(paths) => {
                    if let Some(found) = first_path_match(rule, &label, paths)? {
                        return Ok(Some(hold(
                            CompletionHoldField::Paths,
                            found.0,
                            found.1,
                            None,
                        )));
                    }
                }
                ChangedPaths::Unreadable(reason) => {
                    return Ok(Some(hold(
                        CompletionHoldField::Paths,
                        rule.paths.join(", "),
                        format!("changed files unreadable ({reason})"),
                        None,
                    )));
                }
                ChangedPaths::NoDiff => {}
            }
        }
        if let Some(floor) = rule.complexity_at_least
            && let Some(floor_rank) = completion_hold_complexity_rank(floor)
            && let Some((task, complexity)) = tasks.iter().find_map(|task| {
                let complexity = task.complexity?;
                (completion_hold_complexity_rank(complexity)? >= floor_rank)
                    .then_some((task, complexity))
            })
        {
            return Ok(Some(hold(
                CompletionHoldField::ComplexityAtLeast,
                floor.as_str().to_string(),
                complexity.as_str().to_string(),
                Some(task),
            )));
        }
        if let Some(task) = tasks
            .iter()
            .find(|task| rule.types.contains(&task.task_type))
        {
            return Ok(Some(hold(
                CompletionHoldField::Types,
                task.task_type.to_string(),
                task.task_type.to_string(),
                Some(task),
            )));
        }
        for task in tasks {
            if let Some(tag) = task
                .tags
                .iter()
                .filter_map(|tag| normalize_instruction_tag(tag))
                .find(|tag| rule.tags.contains(tag))
            {
                return Ok(Some(hold(
                    CompletionHoldField::Tags,
                    tag.clone(),
                    tag,
                    Some(task),
                )));
            }
        }
    }
    Ok(None)
}

/// The first changed path a rule's `paths` filter admits, and the including
/// glob that matched it.
fn first_path_match(
    rule: &CompletionHoldRule,
    label: &str,
    paths: &[String],
) -> Result<Option<(String, String)>, OrbitError> {
    let key = format!("{label}.paths");
    let filter = PathFilter::new(&key, &rule.paths)?;
    let Some(path) = paths.iter().find(|path| filter.matches(path)) else {
        return Ok(None);
    };
    // Name the one including glob that admits the path, for the record.
    for glob in rule.paths.iter().filter(|glob| !glob.starts_with('!')) {
        if PathFilter::new(&key, std::slice::from_ref(glob))?.matches(path) {
            return Ok(Some((glob.clone(), path.clone())));
        }
    }
    Ok(Some((rule.paths.join(", "), path.clone())))
}

/// The files a pull request from `candidate` into `base` changes: the diff
/// from their merge base, as the forge shows it. `base` is read as the
/// remote-tracking branch, else the local one (`base_sync = local`).
pub(crate) fn pull_request_changed_paths(
    workspace: &Path,
    base: Option<&str>,
    base_sync_local: bool,
    candidate: Option<&str>,
) -> ChangedPaths {
    let Some(candidate) = candidate.filter(|sha| !sha.trim().is_empty()) else {
        return ChangedPaths::Unreadable("no published head is pinned".to_string());
    };
    let Some(base) = base.map(str::trim).filter(|base| !base.is_empty()) else {
        return ChangedPaths::Unreadable("no base branch is pinned".to_string());
    };
    let branch = base.strip_prefix("origin/").unwrap_or(base);
    let mut refs = Vec::new();
    if !base_sync_local {
        refs.push(format!("refs/remotes/origin/{branch}"));
    }
    refs.push(format!("refs/heads/{branch}"));
    let resolved = refs.into_iter().find(|reference| {
        git_command_success(
            workspace,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{reference}^{{commit}}"),
            ],
        )
        .unwrap_or(false)
    });
    let Some(base_ref) = resolved else {
        return ChangedPaths::Unreadable(format!("base '{branch}' does not resolve"));
    };
    match git_output_paths(
        workspace,
        &[
            "diff",
            "--name-only",
            "-z",
            "--no-renames",
            &format!("{base_ref}...{candidate}"),
            "--",
        ],
    ) {
        Ok(paths) => ChangedPaths::Read(paths),
        Err(error) => ChangedPaths::Unreadable(error.to_string()),
    }
}

/// The files a candidate commit changes against the base commit it was
/// built on: the merge-base diff between the two pinned revisions. This is
/// the local-delivery analogue of [`pull_request_changed_paths`], for a
/// candidate a handoff pins by commit rather than by branch.
pub(crate) fn candidate_commit_changed_paths(
    workspace: &Path,
    base_commit: &str,
    candidate_commit: &str,
) -> ChangedPaths {
    let base_commit = base_commit.trim();
    let candidate_commit = candidate_commit.trim();
    if candidate_commit.is_empty() {
        return ChangedPaths::Unreadable("no candidate commit is pinned".to_string());
    }
    if base_commit.is_empty() {
        return ChangedPaths::Unreadable("no base commit is pinned".to_string());
    }
    match git_output_paths(
        workspace,
        &[
            "diff",
            "--name-only",
            "-z",
            "--no-renames",
            &format!("{base_commit}...{candidate_commit}"),
            "--",
        ],
    ) {
        Ok(paths) => ChangedPaths::Read(paths),
        Err(error) => ChangedPaths::Unreadable(error.to_string()),
    }
}

/// Record `hold` on every task of the bundle: a comment and a
/// `completion_held` history entry carrying the hold. A task whose current
/// hold already records the same match for the same run is left as is, so a
/// re-run of the completion step does not repeat it.
pub(crate) fn record_completion_hold<H: RuntimeHost + ?Sized>(
    host: &H,
    task_ids: &[String],
    hold: &CompletionHold,
) -> Result<(), OrbitError> {
    for task_id in task_ids {
        let mut record = hold.clone();
        if record.matched_task_id.as_deref() == Some(task_id.as_str()) {
            record.matched_task_id = None;
        }
        let task = host.get_task(task_id)?;
        let history = host.get_task_history(task_id)?;
        if current_completion_hold(task.status, &history) == Some(&record) {
            continue;
        }
        let summary = record.summary();
        let now = chrono::Utc::now();
        host.apply_task_automation_update(
            task_id,
            TaskAutomationUpdate {
                append_comments: vec![TaskComment {
                    at: now,
                    by: "system".to_string(),
                    message: format!(
                        "{summary}. Run {} was authorized to complete this task; the \
                         `completion.hold` rule kept it in review for a person to approve.",
                        record.run_id
                    ),
                    operation_id: None,
                }],
                append_history: vec![TaskHistoryEntry {
                    at: now,
                    by: "system".to_string(),
                    event: COMPLETION_HELD_EVENT.to_string(),
                    note: Some(summary),
                    from_status: None,
                    to_status: None,
                    operation_id: None,
                    decision: Some(orbit_types::task::TaskDecision::CompletionHeld(record)),
                }],
                ..TaskAutomationUpdate::default()
            },
        )?;
    }
    Ok(())
}
