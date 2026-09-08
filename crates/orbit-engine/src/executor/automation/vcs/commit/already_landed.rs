//! Conservative revalidation of a task's existing delivery. The worker's
//! structured report is a claim; Git identity, scope, and captured checks must
//! agree before the pipeline can skip creating a new delivery.

use std::collections::BTreeSet;
use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::fs::selector::{anchor_path, overlaps};
use orbit_types::task::{Task, TaskArtifact};
use orbit_types::workflow::{ReviewValidation, ValidationOutcome, ValidationRole};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::context::RuntimeHost;

use super::super::delivery_marker::delivery_markers;
use super::super::git::{git_output, git_output_paths, git_success};
use super::pinned_object_id;

const ARTIFACT: &str = "already-landed.json";

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Evidence {
    schema_version: u32,
    task_id: String,
    run_id: String,
    tested_head: String,
    covering_commit: String,
    covering_task_id: String,
    scope: Value,
    required_commands: Vec<String>,
    validation: Vec<Check>,
    /// One concrete explanation per current acceptance criterion, in order.
    criteria_evidence: Vec<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Check {
    #[serde(flatten)]
    validation: ReviewValidation,
    log_artifact: String,
}

/// Recheck the same contract at commit, promotion, and completion. Returning
/// the complete evidence in each step's ordinary checkpoint preserves exactly
/// what was accepted without rewriting an older run or its successful steps.
pub(in crate::executor::automation::vcs) fn verify<H: RuntimeHost + ?Sized>(
    host: &H,
    task: &Task,
    workspace: &Path,
    run_id: &str,
    tested_head: &str,
) -> Result<Value, OrbitError> {
    let artifacts = host.get_task_artifacts(&task.id)?;
    let report = artifact(&artifacts, ARTIFACT)?;
    let evidence: Evidence = serde_json::from_slice(&report.content)
        .map_err(|error| refused(format!("invalid {ARTIFACT}: {error}")))?;
    if evidence.schema_version != 1
        || evidence.task_id != task.id
        || evidence.covering_task_id != task.id
    {
        return Err(refused(
            "evidence must identify this task and its own covering delivery; cross-task coverage requires reconciliation",
        ));
    }
    if evidence.scope != scope(task, &host.get_task_comments(&task.id)?) {
        return Err(refused(
            "task scope changed; revalidate the current requirements, selectors and comments",
        ));
    }
    ensure_validation_run(host, run_id, &evidence.run_id)?;
    let head = pinned_object_id(&evidence.tested_head)?;
    let covering = pinned_object_id(&evidence.covering_commit)?;
    if head != tested_head || git_output(workspace, &["rev-parse", "HEAD"])? != head {
        return Err(refused(
            "tested HEAD changed; rerun required validation on the pinned current HEAD",
        ));
    }
    if !git_output(
        workspace,
        &["status", "--porcelain", "--untracked-files=all"],
    )?
    .is_empty()
    {
        return Err(refused(
            "worktree is no longer clean; deliver or reconcile the pending changes",
        ));
    }

    git_success(
        workspace,
        &["merge-base", "--is-ancestor", &covering, &head],
    )
    .map_err(|error| {
        refused(format!(
            "covering commit is not a verified ancestor of tested HEAD: {error}"
        ))
    })?;
    let message = git_output(workspace, &["show", "-s", "--format=%B", &covering])?;
    if !delivery_markers(&message).contains(&format!("[{}]", task.id)) {
        return Err(refused(
            "covering commit has no matching task delivery marker",
        ));
    }
    verify_covering_scope(task, workspace, &covering, &head)?;
    let validation_provenance = verify_checks(&evidence, task, &artifacts)?;

    Ok(json!({
        "phase": "commit",
        "decision": "verified_already_landed",
        "committed": false,
        "skipped_no_diff_expected": true,
        "task_id": task.id,
        "job_run_id": run_id,
        "base_sha": head,
        "already_landed": evidence,
        "validation_provenance": validation_provenance,
    }))
}

fn verify_covering_scope(
    task: &Task,
    workspace: &Path,
    covering: &str,
    head: &str,
) -> Result<(), OrbitError> {
    for selector in &task.context_files {
        let anchor = anchor_path(selector).map_err(|error| refused(error.to_string()))?;
        let resolved = workspace
            .join(anchor)
            .canonicalize()
            .map_err(|_| refused("scope anchor is unavailable; reconcile the task selectors"))?;
        if !resolved.starts_with(workspace) {
            return Err(refused("scope anchor is outside the tested workspace"));
        }
    }
    let changed = git_output_paths(
        workspace,
        &["diff", "--name-only", "-z", covering, head, "--"],
    )?;
    if task.context_files.is_empty()
        || changed.iter().any(|path| {
            task.context_files
                .iter()
                .any(|selector| overlaps(selector, path))
        })
    {
        return Err(refused(
            "covering scope is empty or changed since landing; reconcile and revalidate before delivery",
        ));
    }
    let delivered = git_output_paths(
        workspace,
        &[
            "diff-tree",
            "--root",
            "--no-commit-id",
            "--name-only",
            "-r",
            "--first-parent",
            "-m",
            "-z",
            covering,
            "--",
        ],
    )?;
    if !delivered.iter().any(|path| {
        task.context_files
            .iter()
            .any(|selector| overlaps(selector, path))
    }) {
        return Err(refused(
            "covering commit did not change the task's declared scope",
        ));
    }
    Ok(())
}

/// A skip flag alone never authorizes promotion of an untagged task. Require
/// the exact accepted evidence and logs, then recheck live task and Git state.
pub(in crate::executor::automation::vcs) fn verify_handoff<H: RuntimeHost + ?Sized>(
    host: &H,
    tasks: &[Task],
    workspace: &Path,
    run_id: &str,
    checkpoint: &Value,
) -> Result<(), OrbitError> {
    let [task] = tasks else {
        return Err(refused(
            "already-landed completion requires exactly one task",
        ));
    };
    let head = checkpoint["base_sha"].as_str().unwrap_or_default();
    let checked = verify(host, task, workspace, run_id, head)?;
    if checkpoint["decision"] != "verified_already_landed"
        || checkpoint["already_landed"] != checked["already_landed"]
        || checkpoint["validation_provenance"] != checked["validation_provenance"]
    {
        return Err(refused(
            "accepted landing evidence changed; rerun the commit verification step",
        ));
    }
    Ok(())
}

fn scope(task: &Task, comments: &[orbit_types::task::TaskComment]) -> Value {
    json!({
        "title": task.title,
        "description": task.description,
        "acceptance_criteria": task.acceptance_criteria,
        "plan": task.plan,
        "context_files": task.context_files,
        "tags": task.tags,
        "relations": task.relations,
        "required_tools": task.required_tools,
        "type": task.task_type,
        "comments": comments,
    })
}

fn ensure_validation_run<H: RuntimeHost + ?Sized>(
    host: &H,
    run_id: &str,
    evidence_run: &str,
) -> Result<(), OrbitError> {
    let mut current = run_id.to_string();
    let mut seen = BTreeSet::new();
    for _ in 0..64 {
        if !evidence_run.is_empty() && current == evidence_run {
            return Ok(());
        }
        if !seen.insert(current.clone()) {
            break;
        }
        let Some(source) = host
            .get_job_run(&current)?
            .and_then(|run| run.retry_source_run_id)
        else {
            break;
        };
        current = source;
    }
    Err(refused(
        "validation belongs to a different run; capture checks in this run or its recorded retry lineage",
    ))
}

fn verify_checks(
    evidence: &Evidence,
    task: &Task,
    artifacts: &[TaskArtifact],
) -> Result<Vec<Value>, OrbitError> {
    let commands: BTreeSet<_> = evidence.required_commands.iter().collect();
    if commands.is_empty()
        || commands.len() != evidence.required_commands.len()
        || commands.iter().any(|command| command.trim().is_empty())
        || evidence.validation.len() != commands.len()
        || task.acceptance_criteria.is_empty()
        || evidence.criteria_evidence.len() != task.acceptance_criteria.len()
        || evidence
            .criteria_evidence
            .iter()
            .any(|item| item.trim().is_empty())
    {
        return Err(refused(
            "list every required workspace/task check and concrete evidence for every acceptance criterion",
        ));
    }
    let mut checked = BTreeSet::new();
    let mut logs = Vec::new();
    for check in &evidence.validation {
        let validation = &check.validation;
        if validation.outcome != ValidationOutcome::Passed
            || validation.role != ValidationRole::Required
            || !commands.contains(&validation.command)
            || !checked.insert(&validation.command)
        {
            return Err(refused(
                "required validation is missing, duplicated, failed, denied or not run",
            ));
        }
        let log = artifact(artifacts, &check.log_artifact)?;
        let provenance: Value = serde_json::from_slice(&log.content).map_err(|error| {
            refused(format!(
                "invalid validation log {}: {error}",
                check.log_artifact
            ))
        })?;
        if provenance["run_id"] != evidence.run_id
            || provenance["tested_head"] != evidence.tested_head
            || provenance["command"] != validation.command
            || provenance["exit_code"] != 0
            || provenance["output"].as_str().is_none()
        {
            return Err(refused(
                "validation log must capture the exact run, tested HEAD, command, zero exit code and output",
            ));
        }
        logs.push(provenance);
    }
    Ok(logs)
}

fn artifact<'a>(artifacts: &'a [TaskArtifact], path: &str) -> Result<&'a TaskArtifact, OrbitError> {
    artifacts.iter().find(|artifact| artifact.path == path)
        .ok_or_else(|| refused(format!("missing task artifact {path}; attach structured landing proof and required validation logs")))
}

fn refused(reason: impl std::fmt::Display) -> OrbitError {
    OrbitError::Execution(format!("already_landed_unverified: {reason}"))
}
