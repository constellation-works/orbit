pub(super) mod already_landed;
mod author;
mod git_ops;
mod message;
mod scope;
mod summary;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_types::task::NO_DIFF_EXPECTED_TAG;
use serde_json::{Value, json};

use crate::context::RuntimeHost;

use super::super::input::{canonicalize_existing_dir, input_string_field, required_job_run_id};
use super::failure::commit_head_matches_failure_handoff;
use super::git::{git_output, git_success};
use super::handoff::reject_failed_delivery;
use author::{append_co_author_trailers, commit_author_for_tasks, reviewer_author};
use git_ops::{
    ensure_named_branch, ensure_no_unmerged_changes, git_commit_as, git_commit_with_identity,
    stage_paths, staged_changed_files,
};
use message::{batch_commit_message, finalize_commit_message, task_commit_message};
use scope::{changed_files_for_task, collect_worktree_changes, filter_changed_files_for_task};
use summary::ensure_durable_execution_summary;

pub(in crate::executor::automation) fn git_commit<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
) -> Result<Value, OrbitError> {
    let scope = input.get("scope").and_then(Value::as_str).unwrap_or("all");
    match scope {
        "per_task" => commit_task_artifact_changes(host, input),
        "per_task_finalize" => commit_finalize_artifact_changes(host, input),
        "all" => commit_batch_changes(host, input),
        other => Err(OrbitError::InvalidInput(format!(
            "git_commit: unknown scope '{other}'; expected per_task, per_task_finalize, or all"
        ))),
    }
}

pub(super) fn commit_task_artifact_changes<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
) -> Result<Value, OrbitError> {
    let batch_id = required_job_run_id(input, "commit_task_artifact_changes")?;
    let explicit_completed_task_ids = completed_task_ids_field(input);
    if explicit_completed_task_ids
        .as_ref()
        .is_some_and(|task_ids| task_ids.is_empty())
    {
        return Ok(json!({
            "committed_task_ids": [],
            "skipped_task_ids": [],
        }));
    }

    let fallback_batch_tasks = if explicit_completed_task_ids.is_none() {
        Some(host.list_tasks_filtered(None, None, None, Some(batch_id), None, None)?)
    } else {
        None
    };
    if fallback_batch_tasks
        .as_ref()
        .is_some_and(|batch_tasks| batch_tasks.is_empty())
    {
        return Ok(json!({
            "committed_task_ids": [],
            "skipped_task_ids": [],
        }));
    }

    let workspace_path = resolve_workspace_path(host, input, batch_id)?;
    ensure_named_branch(&workspace_path)?;
    ensure_no_unmerged_changes(&workspace_path)?;
    let resolved_model = host.resolved_crew_model(batch_id)?;

    let task_ids = match explicit_completed_task_ids {
        Some(task_ids) => task_ids,
        None => fallback_batch_tasks
            .unwrap_or_default()
            .into_iter()
            .map(|task| task.id)
            .collect(),
    };

    let mut committed_task_ids = Vec::new();
    let mut skipped_task_ids = Vec::new();

    for task_id in task_ids {
        let task = host.get_task(&task_id)?;
        let changed_files = changed_files_for_task(&workspace_path, &task)?;
        if changed_files.is_empty() {
            skipped_task_ids.push(task_id);
            continue;
        }

        stage_paths(&workspace_path, &changed_files)?;
        let staged_files = staged_changed_files(&workspace_path)?;
        if staged_files.is_empty() {
            skipped_task_ids.push(task.id);
            continue;
        }

        let message = task_commit_message(&task);
        git_commit_with_identity(&workspace_path, &message, resolved_model.as_deref())?;
        committed_task_ids.push(task.id);
    }

    Ok(json!({
        "workspace_path": workspace_path.to_string_lossy().to_string(),
        "committed_task_ids": committed_task_ids,
        "skipped_task_ids": skipped_task_ids,
    }))
}

pub(super) fn commit_finalize_artifact_changes<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
) -> Result<Value, OrbitError> {
    let batch_id = required_job_run_id(input, "commit_finalize_artifact_changes")?;
    let batch_tasks = host.list_tasks_filtered(None, None, None, Some(batch_id), None, None)?;
    if batch_tasks.is_empty() {
        return Ok(json!({}));
    }

    let workspace_path = resolve_workspace_path(host, input, batch_id)?;
    ensure_named_branch(&workspace_path)?;
    ensure_no_unmerged_changes(&workspace_path)?;

    let changed_files = collect_worktree_changes(&workspace_path)?;
    if changed_files.is_empty() {
        return Ok(json!({}));
    }

    let mut affected_tasks = Vec::new();
    let mut files_to_commit = BTreeSet::new();
    for task in batch_tasks {
        let task_files = filter_changed_files_for_task(&changed_files, &workspace_path, &task);
        if task_files.is_empty() {
            continue;
        }
        files_to_commit.extend(task_files);
        affected_tasks.push(task);
    }

    if affected_tasks.is_empty() {
        return Ok(json!({}));
    }

    let files_to_commit: Vec<String> = files_to_commit.into_iter().collect();
    stage_paths(&workspace_path, &files_to_commit)?;
    let staged_files = staged_changed_files(&workspace_path)?;
    if staged_files.is_empty() {
        return Ok(json!({}));
    }

    let mut message = finalize_commit_message(&affected_tasks);
    let (_, coauthors) = commit_author_for_tasks(&affected_tasks);
    append_co_author_trailers(&mut message, &coauthors);
    let resolved_model = host.resolved_crew_model(batch_id)?;
    git_commit_with_identity(&workspace_path, &message, resolved_model.as_deref())?;

    Ok(json!({
        "workspace_path": workspace_path.to_string_lossy().to_string(),
        "committed_task_ids": affected_tasks.into_iter().map(|task| task.id).collect::<Vec<_>>(),
        "committed_files": staged_files,
    }))
}

pub(super) fn commit_batch_changes<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
) -> Result<Value, OrbitError> {
    let batch_id = required_job_run_id(input, "commit_batch_changes")?;
    let batch_tasks = host.list_tasks_filtered(None, None, None, Some(batch_id), None, None)?;
    let [task] = batch_tasks.as_slice() else {
        return Err(OrbitError::InvalidInput(format!(
            "commit_batch_changes expected exactly one task for job_run_id '{batch_id}', got {}",
            batch_tasks.len()
        )));
    };

    let workspace_path = resolve_workspace_path(host, input, batch_id)?;
    ensure_named_branch(&workspace_path)?;

    ensure_no_unmerged_changes(&workspace_path)?;

    // ORB-10603: the summary the gate reads is durable state, and nothing in the
    // pipeline filled it when the implementing agent skipped the instruction to
    // persist one. Derive it read-only from the change about to be delivered —
    // never from the agent's advisory response envelope — and only when the
    // agent persisted nothing of its own.
    let task = ensure_durable_execution_summary(host, task.clone(), &workspace_path, batch_id)?;

    // ORB-10313: fail closed on the durable execution outcome before staging
    // files, mutating the index, or committing. Only read-only resolution and
    // validation run ahead of it; the gate itself is unchanged, and an empty or
    // underivable summary still refuses delivery here.
    reject_failed_delivery(&task)?;

    // ADR-0219: an explicitly side-effect-only task skips this phase instead of
    // failing it. Read the tag before any gate so the carve-out is reachable
    // from every failure branch below, not just the empty-stage one (ORB-10380).
    let no_diff_expected = task.tags.iter().any(|tag| tag == NO_DIFF_EXPECTED_TAG);
    let allow_empty = input
        .get("allow_empty")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let allow_moved_head = input
        .get("allow_moved_head")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let (base_sha, head_moved) = match validate_pinned_head(&workspace_path, input)? {
        PinnedHead::Matched(base_sha) => (Some(base_sha), false),
        PinnedHead::Unpinned => (None, false),
        PinnedHead::Changed { base_sha, head_sha } => {
            if no_diff_expected {
                return Ok(skipped_no_diff_expected_result(&task.id));
            }
            let preserved_failure_head = commit_head_matches_failure_handoff(
                host,
                input,
                &task,
                batch_id,
                &workspace_path,
                &base_sha,
                &head_sha,
            )?;
            if !preserved_failure_head && !allow_moved_head {
                return Err(changed_head_error(
                    &task.id,
                    &workspace_path,
                    &base_sha,
                    &head_sha,
                ));
            }
            (Some(base_sha), true)
        }
    };

    // A proposed ADR the run allocated lives in an ignored partition, so
    // `git add --all` below would skip it and ship the code without the
    // decision documenting it. Hand it off first: this is the step that can
    // fail on read-only worktree metadata, and going first means that failure
    // names the bundle instead of surfacing as a bare `git add` error.

    git_success(&workspace_path, &["add", "--all", "--", "."])?;

    let changed_files = staged_changed_files(&workspace_path)?;
    if changed_files.is_empty() {
        // ORB-10380: no failure path mutates worktree state on its way out.
        // `git add --all` staged nothing here, so the index already matches
        // HEAD and the former `git reset HEAD` was both pointless and a
        // mutation performed while erroring.
        if head_moved {
            // Child pipelines already advanced HEAD. There is a diff to
            // deliver; it just is not sitting uncommitted.
            return Ok(already_committed_result(&task.id, base_sha.as_deref()));
        }
        if no_diff_expected || allow_empty {
            return Ok(skipped_no_diff_expected_result(&task.id));
        }
        if input.get("verify_already_landed").and_then(Value::as_bool) == Some(true)
            && let Some(base_sha) = base_sha.as_deref()
        {
            return already_landed::verify(
                host,
                &task,
                &workspace_path,
                input_string_field(input, "run_id")
                    .as_deref()
                    .unwrap_or(batch_id),
                base_sha,
            )
            .map_err(|error| {
                match empty_stage_error(&task.id, &workspace_path, Some(base_sha)) {
                    Ok(OrbitError::Execution(observed)) => {
                        OrbitError::Execution(format!("{observed}; {error}"))
                    }
                    Ok(observed) | Err(observed) => observed,
                }
            });
        }
        return Err(empty_stage_error(
            &task.id,
            &workspace_path,
            base_sha.as_deref(),
        )?);
    }

    let message = batch_commit_message(&task);
    let resolved_model = host.resolved_crew_model(batch_id)?;

    git_commit_with_identity(&workspace_path, &message, resolved_model.as_deref())?;
    let commit_sha = git_output(&workspace_path, &["rev-parse", "HEAD"])?;
    let mut result = json!({
        "phase": "commit",
        "decision": "performed",
        "committed": true,
        "commit_sha": commit_sha.trim(),
        "job_run_id": batch_id,
        "skipped_no_diff_expected": false,
        "task_id": task.id,
    });
    if let Some(base_sha) = base_sha {
        result["base_sha"] = json!(base_sha);
    }
    Ok(result)
}

/// Stage every worktree change for a reviewer repair commit [ORB-11333].
pub(super) fn stage_everything(workspace_path: &Path) -> Result<(), OrbitError> {
    ensure_named_branch(workspace_path)?;
    ensure_no_unmerged_changes(workspace_path)?;
    git_success(workspace_path, &["add", "--all", "--", "."])
}

/// The staged paths, relative to the worktree.
pub(super) fn staged_paths(workspace_path: &Path) -> Result<Vec<String>, OrbitError> {
    staged_changed_files(workspace_path)
}

/// Commit the staged reviewer repairs under the reviewer's own identity.
pub(super) fn commit_reviewer_repairs_in(
    workspace_path: &Path,
    reviewer_model: &str,
    message: &str,
) -> Result<(), OrbitError> {
    git_commit_as(workspace_path, message, &reviewer_author(reviewer_model))
}

/// Commit a terminally-failed shipment's dirty candidate without consulting
/// the normal success-summary delivery gate. ADR-0246 confines this bypass to
/// the failure handoff, which blocks rather than promotes the task.
pub(super) fn commit_failure_candidate<H: RuntimeHost + ?Sized>(
    host: &H,
    run_id: &str,
    workspace_path: &Path,
    task: &orbit_types::task::Task,
) -> Result<(String, Vec<String>), OrbitError> {
    ensure_named_branch(workspace_path)?;
    ensure_no_unmerged_changes(workspace_path)?;
    git_success(workspace_path, &["add", "--all", "--", "."])?;
    let changed_files = staged_changed_files(workspace_path)?;
    if !changed_files.is_empty() {
        let message = batch_commit_message(task);
        let resolved_model = host.resolved_crew_model(run_id)?;
        git_commit_with_identity(workspace_path, &message, resolved_model.as_deref())?;
    }
    let head_sha = git_output(workspace_path, &["rev-parse", "--verify", "HEAD^{commit}"])?;
    Ok((head_sha.trim().to_string(), changed_files))
}

/// Outcome of comparing `input.base_sha` with the worktree's current HEAD.
enum PinnedHead {
    /// No base was pinned by the caller, so this step attributes no history.
    Unpinned,
    Matched(String),
    Changed {
        base_sha: String,
        head_sha: String,
    },
}

/// Compare HEAD with the immutable checkpoint `worktree_setup` pinned for this
/// run without traversing or interpreting provider-created history.
///
/// ORB-10380: the input is a commit id resolved once at worktree creation, not
/// a ref name. `refs/remotes/origin/<base>` is shared by every worktree off the
/// one `.git`, so any sibling run's fetch or any merge moves it mid-run; a
/// commit step that re-resolved the name failed every older in-flight run by
/// construction. Nothing here resolves a ref.
fn validate_pinned_head(workspace_path: &Path, input: &Value) -> Result<PinnedHead, OrbitError> {
    let Some(pinned) = input_string_field(input, "base_sha") else {
        return Ok(PinnedHead::Unpinned);
    };
    let pinned = pinned_object_id(&pinned)?;

    let base_sha = git_output(
        workspace_path,
        &["rev-parse", "--verify", &format!("{pinned}^{{commit}}")],
    )
    .map_err(|error| {
        OrbitError::Execution(format!(
            "commit_batch_changes: pinned base commit '{pinned}' is not present in worktree \
             '{}': {error}",
            workspace_path.display()
        ))
    })?;
    let head_sha = git_output(workspace_path, &["rev-parse", "--verify", "HEAD^{commit}"])?;

    if base_sha == head_sha {
        return Ok(PinnedHead::Matched(base_sha));
    }

    Ok(PinnedHead::Changed { base_sha, head_sha })
}

/// Reject anything that is not a full Git object id.
///
/// The commit step's contract is a base pinned at worktree setup; accepting a
/// ref name here would silently restore the moving-base failure (ORB-10380).
fn pinned_object_id(value: &str) -> Result<String, OrbitError> {
    let candidate = value.trim();
    let is_object_id =
        matches!(candidate.len(), 40 | 64) && candidate.chars().all(|c| c.is_ascii_hexdigit());
    if !is_object_id {
        return Err(OrbitError::InvalidInput(format!(
            "git_commit: input.base_sha must be the full commit id pinned by worktree_setup, got \
             '{value}'; the commit step never resolves a ref name because the shared base ref \
             moves while a run is in flight"
        )));
    }
    Ok(candidate.to_ascii_lowercase())
}

fn skipped_no_diff_expected_result(task_id: &str) -> Value {
    json!({
        "phase": "commit",
        "decision": "skipped_no_diff_expected",
        "committed": false,
        "skipped_no_diff_expected": true,
        "task_id": task_id,
    })
}

fn already_committed_result(task_id: &str, base_sha: Option<&str>) -> Value {
    let mut result = json!({
        "phase": "commit",
        "decision": "already_committed",
        "committed": false,
        "skipped_no_diff_expected": false,
        "task_id": task_id,
    });
    if let Some(base_sha) = base_sha {
        result["base_sha"] = json!(base_sha);
    }
    result
}

/// The worktree carries no committable work. Reports only what was observed —
/// this message shares no wording with [`unrelated_history_error`] so a reader
/// can tell the two conditions apart (ORB-10380).
fn empty_stage_error(
    task_id: &str,
    workspace_path: &Path,
    base_sha: Option<&str>,
) -> Result<OrbitError, OrbitError> {
    let counts = worktree_status_counts(workspace_path)?;
    let head_sha = git_output(workspace_path, &["rev-parse", "--verify", "HEAD^{commit}"])?;
    let base = match base_sha {
        Some(base_sha) => format!("pinned base {base_sha}"),
        None => "no pinned base in this step's input".to_string(),
    };
    Ok(OrbitError::Execution(format!(
        "commit_batch_changes: nothing to commit for task '{task_id}' in worktree '{}'. \
         Observed after `git add --all`: {} staged, {} unstaged, {} untracked file(s); \
         HEAD {head_sha}; {base}. Orbit did not inspect, stage, or reset any other checkout",
        workspace_path.display(),
        counts.staged,
        counts.unstaged,
        counts.untracked
    )))
}

fn changed_head_error(
    task_id: &str,
    workspace_path: &Path,
    base_sha: &str,
    head_sha: &str,
) -> OrbitError {
    OrbitError::Execution(format!(
        "commit_batch_changes: worktree_head_changed for task '{task_id}' in '{}'. \
         Observed pinned base {base_sha} and HEAD {head_sha}; the provider boundary must leave \
         the immutable setup checkpoint at HEAD. Orbit did not stage, reset, or adopt history.",
        workspace_path.display()
    ))
}

#[derive(Default)]
struct WorktreeStatusCounts {
    staged: usize,
    unstaged: usize,
    untracked: usize,
}

fn worktree_status_counts(workspace_path: &Path) -> Result<WorktreeStatusCounts, OrbitError> {
    let status = git_output(
        workspace_path,
        &["status", "--porcelain=v1", "--untracked-files=all"],
    )?;
    let mut counts = WorktreeStatusCounts::default();
    for line in status.lines() {
        let mut code = line.chars();
        let (Some(index_state), Some(worktree_state)) = (code.next(), code.next()) else {
            continue;
        };
        if index_state == '?' {
            counts.untracked += 1;
            continue;
        }
        if index_state != ' ' {
            counts.staged += 1;
        }
        if worktree_state != ' ' {
            counts.unstaged += 1;
        }
    }
    Ok(counts)
}

fn resolve_workspace_path<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
    batch_id: &str,
) -> Result<PathBuf, OrbitError> {
    match input_string_field(input, "workspace_path") {
        Some(ws) => canonicalize_existing_dir(&ws, "workspace_path"),
        None => {
            let repo_root_str = host.repo_root()?;
            let repo_root = Path::new(&repo_root_str);
            super::worktree::resolve_shared_worktree_path(repo_root, batch_id)
        }
    }
}

fn completed_task_ids_field(input: &Value) -> Option<Vec<String>> {
    let items = input.get("completed_task_ids")?.as_array()?;
    Some(
        items
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>(),
    )
}

#[cfg(test)]
mod tests;
