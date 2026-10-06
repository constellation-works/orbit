//! The `git_commit` scopes (per-task, per-task finalize, single-task batch)
//! and the failure-handoff candidate commit.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_types::task::{ContextWideningStep, NO_DIFF_EXPECTED_TAG};
use serde_json::{Value, json};

use crate::context::RuntimeHost;

use super::super::super::input::{
    canonicalize_existing_dir, input_string_field, required_job_run_id,
};
use super::super::failure::commit_head_matches_failure_handoff;
use super::super::git::git_output;
use super::super::handoff::{
    claimed_attempt_summary, reject_failed_attempt, reject_failed_delivery,
};
use super::super::pr::meaningful_execution_summary;
use super::author::{append_co_author_trailers, commit_author_for_tasks};
use super::checkpoint::{
    PinnedHead, head_descends_from_pin, validate_pinned_head, verify_clean_tree,
};
use super::diagnostics::{changed_head_error, empty_stage_error};
use super::git_ops::{
    ensure_named_branch, ensure_no_unmerged_changes, git_commit_paths_with_identity,
    git_commit_with_identity, stage_paths, staged_changed_files,
};
use super::message::{batch_commit_message, finalize_commit_message, task_commit_message};
use super::scope::{NewPathPolicy, attribute_candidate_paths, task_candidate_paths};
use super::summary::ensure_durable_execution_summary;

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
        Some(host.list_run_tasks(batch_id)?)
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
    let task_ids = match explicit_completed_task_ids {
        Some(task_ids) => task_ids,
        None => fallback_batch_tasks
            .unwrap_or_default()
            .into_iter()
            .map(|task| task.id)
            .collect(),
    };
    let tasks = task_ids
        .iter()
        .map(|task_id| host.get_task(task_id))
        .collect::<Result<Vec<_>, _>>()?;
    let resolved_model = host.resolved_crew_model(batch_id)?;
    // Multi-task scopes run only in local pipelines. Each path is committed
    // with exactly one task: the one whose agent changed it when selectors
    // alone are ambiguous, widening that task's selectors when none cover it.
    let candidate_paths = task_candidate_paths(&workspace_path, NewPathPolicy::Owner)?;
    let mut assigned = attribute_candidate_paths(
        host,
        batch_id,
        ContextWideningStep::Implement,
        "git_commit",
        &candidate_paths,
        &workspace_path,
        &tasks,
        true,
    );

    let mut committed_task_ids = Vec::new();
    let mut skipped_task_ids = Vec::new();

    for task in tasks {
        let changed_files = assigned
            .remove(&task.id)
            .unwrap_or_default()
            .into_iter()
            .collect::<Vec<_>>();
        if changed_files.is_empty() {
            skipped_task_ids.push(task.id);
            continue;
        }

        stage_paths(&workspace_path, &changed_files)?;
        let message = task_commit_message(&task);
        git_commit_paths_with_identity(
            &workspace_path,
            &message,
            resolved_model.as_deref(),
            &changed_files,
        )?;
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
    let batch_tasks = host.list_run_tasks(batch_id)?;
    if batch_tasks.is_empty() {
        return Ok(json!({}));
    }

    let workspace_path = resolve_workspace_path(host, input, batch_id)?;
    ensure_named_branch(&workspace_path)?;
    ensure_no_unmerged_changes(&workspace_path)?;

    let changed_files = task_candidate_paths(&workspace_path, NewPathPolicy::Owner)?;
    if changed_files.is_empty() {
        return Ok(json!({}));
    }
    let assigned = attribute_candidate_paths(
        host,
        batch_id,
        ContextWideningStep::Implement,
        "git_commit",
        &changed_files,
        &workspace_path,
        &batch_tasks,
        false,
    );

    let mut affected_tasks = Vec::new();
    let mut files_to_commit = BTreeSet::new();
    for task in batch_tasks {
        let Some(task_files) = assigned.get(&task.id).filter(|files| !files.is_empty()) else {
            continue;
        };
        files_to_commit.extend(task_files.iter().cloned());
        affected_tasks.push(task);
    }

    if affected_tasks.is_empty() {
        return Ok(json!({}));
    }

    let files_to_commit: Vec<String> = files_to_commit.into_iter().collect();
    stage_paths(&workspace_path, &files_to_commit)?;
    let mut message = finalize_commit_message(&affected_tasks);
    let (_, coauthors) = commit_author_for_tasks(&affected_tasks);
    append_co_author_trailers(&mut message, &coauthors);
    let resolved_model = host.resolved_crew_model(batch_id)?;
    git_commit_paths_with_identity(
        &workspace_path,
        &message,
        resolved_model.as_deref(),
        &files_to_commit,
    )?;

    Ok(json!({
        "workspace_path": workspace_path.to_string_lossy().to_string(),
        "committed_task_ids": affected_tasks.into_iter().map(|task| task.id).collect::<Vec<_>>(),
        "committed_files": files_to_commit,
    }))
}

pub(super) fn commit_batch_changes<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
) -> Result<Value, OrbitError> {
    let batch_id = required_job_run_id(input, "commit_batch_changes")?;
    let batch_tasks = host.list_run_tasks(batch_id)?;
    let [task] = batch_tasks.as_slice() else {
        return Err(OrbitError::InvalidInput(format!(
            "commit_batch_changes expected exactly one task for job_run_id '{batch_id}', got {}",
            batch_tasks.len()
        )));
    };

    let workspace_path = resolve_workspace_path(host, input, batch_id)?;
    ensure_named_branch(&workspace_path)?;

    ensure_no_unmerged_changes(&workspace_path)?;

    // ORB-13756: the claim's worker binding, not step input, marks a claimed
    // leaf, whose new paths the owner must be able to accept as widening.
    let new_path_policy = if host
        .worker_invocation()
        .is_some_and(|binding| binding.task_id == task.id)
    {
        NewPathPolicy::Claimed
    } else {
        NewPathPolicy::Owner
    };
    let task = if claimed_attempt_summary(host, input, task).is_some() {
        // ORB-13755: a claimed leaf delivers this attempt, whose summary lives
        // in the implementer output the pipeline handed this step, not in the
        // owner's record (which a previous attempt may have left reporting
        // failure). The gate judges that output before any Git mutation, and
        // nothing is derived or written here: the owner's summary is the one
        // its handoff acceptance records.
        reject_failed_attempt(host, input, task)?;
        task.clone()
    } else {
        // ORB-10603: the summary the gate reads is durable state, and nothing in
        // the pipeline filled it when the implementing agent skipped the
        // instruction to persist one. Derive it read-only from the change about
        // to be delivered — never from the agent's advisory response envelope —
        // and only when the agent persisted nothing of its own.
        let task = ensure_durable_execution_summary(host, task.clone(), &workspace_path, batch_id)?;

        // ORB-10313: fail closed on the durable execution outcome before staging
        // files, mutating the index, or committing. Only read-only resolution and
        // validation run ahead of it; the gate itself is unchanged, and an empty
        // or underivable summary still refuses delivery here.
        if meaningful_execution_summary(&task.execution_summary).is_none() {
            // Derivation found no uncommitted change to describe, so the agent
            // finished without leaving one and without saying why. Name that
            // outcome rather than only the missing field, and what resolves it.
            return Err(OrbitError::Execution(format!(
                "task '{}' requires a meaningful persisted execution_summary before delivery; \
                 the implementing agent recorded none and the worktree holds no uncommitted \
                 change to derive one from. A task that needs no change must say so in its \
                 summary and attach no-diff evidence; otherwise re-run it",
                task.id
            )));
        }
        reject_failed_delivery(&task)?;
        task
    };

    // ADR-0219: an explicitly side-effect-only task may skip a *clean* commit
    // phase instead of failing it. ORB-12683: a descendant HEAD means the run
    // already committed corrections, so the tag must not skip before the tree
    // is inspected — fall through to already_committed / leftover-work handling.
    // ORB-12690: the tag is not an unconditional allow_moved_head. Bypass
    // changed_head_error only when HEAD is a descendant of the pinned base;
    // unrelated or otherwise non-descendant history still fails closed.
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
            let preserved_failure_head = commit_head_matches_failure_handoff(
                host,
                input,
                &task,
                batch_id,
                &workspace_path,
                &base_sha,
                &head_sha,
            )?;
            let tagged_descendant =
                no_diff_expected && head_descends_from_pin(&workspace_path, &base_sha, &head_sha)?;
            if !preserved_failure_head && !allow_moved_head && !tagged_descendant {
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

    // Every tracked change and new path outside scratch is delivery: agents
    // may change any path the work requires. Resolve and validate that set
    // before mutating the index, then stage exactly those paths.
    let candidate_paths = task_candidate_paths(&workspace_path, new_path_policy)?;
    let candidate_paths = candidate_paths.into_iter().collect::<Vec<_>>();
    stage_paths(&workspace_path, &candidate_paths)?;

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
            return verify_clean_tree(
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

    // ORB-14247: a `no-diff-expected` task that still has a diff is committed
    // like any other shipment. The tag only skips a clean stage. It does not
    // hold context locks, so a concurrent task may edit the same files;
    // `sync_base` (`git_rebase`) is the conflict boundary and reports
    // `RecoverableVcsConflict` the same way it does for every other task.

    // Widen the task's selectors over every delivered path they do not yet
    // cover. A claimed leaf's host widens nothing: the owner does at handoff.
    attribute_candidate_paths(
        host,
        batch_id,
        ContextWideningStep::Implement,
        "git_commit",
        &changed_files.iter().cloned().collect(),
        &workspace_path,
        std::slice::from_ref(&task),
        true,
    );
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

/// Commit a terminally-failed shipment's dirty candidate without consulting
/// the normal success-summary delivery gate. ADR-0246 confines this bypass to
/// the failure handoff, which blocks rather than promotes the task.
pub(in crate::executor::automation::vcs) fn commit_failure_candidate<H: RuntimeHost + ?Sized>(
    host: &H,
    run_id: &str,
    workspace_path: &Path,
    task: &orbit_types::task::Task,
) -> Result<(String, Vec<String>), OrbitError> {
    ensure_named_branch(workspace_path)?;
    ensure_no_unmerged_changes(workspace_path)?;
    // Only `pr_failure_handoff` reaches this, and no claimed pipeline runs it.
    let candidate_paths = task_candidate_paths(workspace_path, NewPathPolicy::Owner)?;
    stage_paths(
        workspace_path,
        &candidate_paths.into_iter().collect::<Vec<_>>(),
    )?;
    let changed_files = staged_changed_files(workspace_path)?;
    if !changed_files.is_empty() {
        attribute_candidate_paths(
            host,
            run_id,
            ContextWideningStep::Implement,
            "pr_failure_handoff",
            &changed_files.iter().cloned().collect(),
            workspace_path,
            std::slice::from_ref(task),
            true,
        );
        let message = batch_commit_message(task);
        let resolved_model = host.resolved_crew_model(run_id)?;
        git_commit_with_identity(workspace_path, &message, resolved_model.as_deref())?;
    }
    let head_sha = git_output(workspace_path, &["rev-parse", "--verify", "HEAD^{commit}"])?;
    Ok((head_sha.trim().to_string(), changed_files))
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
            super::super::worktree::resolve_shared_worktree_path(repo_root, batch_id)
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
