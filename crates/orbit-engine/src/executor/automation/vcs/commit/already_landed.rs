//! Conservative revalidation of a task's existing delivery. The worker's
//! structured report is a claim; Git identity, scope, and captured checks must
//! agree before the pipeline can skip creating a new delivery.
//!
//! `covering_task_id` names this task (same-task retry) or a sibling whose
//! covering commit already landed the required change on the pinned HEAD.

use std::collections::BTreeSet;
use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::fs::selector::{anchor_path, overlaps};
use orbit_types::task::{Task, TaskArtifact};
use orbit_types::workflow::handoff::{
    AlreadyLandedEvidence as Evidence, already_landed_scope as scope,
};
use orbit_types::workflow::{ValidationOutcome, ValidationRole};
use serde_json::{Value, json};

use crate::context::RuntimeHost;

use super::super::delivery_marker::delivery_markers;
use super::super::git::{git_output, git_output_paths, git_success};
use super::checkpoint::pinned_object_id;
use super::no_diff;

pub(super) const ARTIFACT: &str = "already-landed.json";

/// Checkpoint decision for a verified covering delivery.
pub(super) const DECISION: &str = "verified_already_landed";

/// Recheck the same contract at commit, promotion, and completion. Returning
/// the complete evidence in each step's ordinary checkpoint preserves exactly
/// what was accepted without rewriting an older run or its successful steps.
pub(super) fn verify<H: RuntimeHost + ?Sized>(
    host: &H,
    task: &Task,
    workspace: &Path,
    run_id: &str,
    tested_head: &str,
) -> Result<Value, OrbitError> {
    if git_output(workspace, &["rev-parse", "HEAD"])? != tested_head {
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
    if host.get_task_artifact(&task.id, ARTIFACT)?.is_none()
        && host
            .get_task_artifact(&task.id, no_diff::ARTIFACT)?
            .is_none()
    {
        return Err(refused(format!(
            "missing task artifact {ARTIFACT} or {no_diff}; for a task that correctly changed nothing, attach {no_diff} naming this task, its run or retry lineage, the pinned HEAD as tested_head, a reason, and zero-exit validation logs carrying run_id, tested_head, command, exit_code and output; for work a covering commit delivered, attach {ARTIFACT} with structured landing proof and required validation logs",
            no_diff = no_diff::ARTIFACT,
        )));
    }
    verify_at_revision(host, task, workspace, run_id, tested_head)
}

/// Verify immutable Git objects and task artifacts. The owner uses this on
/// its observed base without checking out the executor's branch.
fn verify_at_revision<H: RuntimeHost + ?Sized>(
    host: &H,
    task: &Task,
    workspace: &Path,
    run_id: &str,
    tested_head: &str,
) -> Result<Value, OrbitError> {
    let report = artifact(host, &task.id, ARTIFACT)?;
    let evidence: Evidence = serde_json::from_slice(&report.content).map_err(|error| {
        let diagnostic = error.to_string();
        let hints = shape_hints(&diagnostic, &report.content);
        refused(format!("invalid {ARTIFACT}: {diagnostic}{hints}"))
    })?;
    if evidence.schema_version != 1
        || evidence.task_id != task.id
        || evidence.covering_task_id.trim().is_empty()
    {
        return Err(refused(
            "evidence must identify this task and a covering delivery",
        ));
    }
    if evidence.scope != scope(task, &host.get_task_comments(&task.id)?) {
        return Err(refused(
            "task scope changed; revalidate the current requirements, selectors and comments",
        ));
    }
    if !run_in_lineage(host, run_id, &evidence.run_id)? {
        return Err(refused(
            "validation belongs to a different run; capture checks in this run or its recorded retry lineage",
        ));
    }
    let head = pinned_object_id(&evidence.tested_head)?;
    let covering = pinned_object_id(&evidence.covering_commit)?;
    if head != tested_head {
        return Err(refused("tested HEAD is not the owner-observed base"));
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
    if !delivery_markers(&message).contains(&format!("[{}]", evidence.covering_task_id)) {
        return Err(refused(
            "covering commit has no matching task delivery marker",
        ));
    }
    verify_covering_scope(task, workspace, &covering, &head)?;
    let validation_provenance = verify_checks(host, &evidence, task)?;

    Ok(json!({
        "phase": "commit",
        "decision": DECISION,
        "committed": false,
        "skipped_no_diff_expected": true,
        "task_id": task.id,
        "job_run_id": run_id,
        "base_sha": head,
        "already_landed": evidence,
        "validation_provenance": validation_provenance,
    }))
}

/// Field-contract reminders for a report that failed to decode. Type and role
/// mistakes are located structurally, since serde's message names neither the
/// field nor, under `flatten`, the accepted role vocabulary.
fn shape_hints(diagnostic: &str, content: &[u8]) -> String {
    let mut hints = String::new();
    if diagnostic.contains("missing field `command`") {
        hints.push_str("; each validation[] element must flatten command, outcome, role, and log_artifact as sibling fields");
    }
    let Ok(report) = serde_json::from_slice::<Value>(content) else {
        return hints;
    };
    if report["criteria_evidence"]
        .as_array()
        .is_some_and(|items| items.iter().any(|item| !item.is_string()))
    {
        hints.push_str("; criteria_evidence must be an array of non-empty strings, one per acceptance criterion in order");
    }
    let unknown_role = report["validation"].as_array().is_some_and(|checks| {
        checks.iter().any(|check| {
            check
                .get("role")
                .is_some_and(|role| serde_json::from_value::<ValidationRole>(role.clone()).is_err())
        })
    });
    if unknown_role {
        hints.push_str("; validation[] role must be one of required, expected_failure, excluded, superseded, diagnostic (use required, with outcome passed, for each required command)");
    }
    hints
}

fn verify_covering_scope(
    task: &Task,
    workspace: &Path,
    covering: &str,
    head: &str,
) -> Result<(), OrbitError> {
    let canonical_workspace = workspace
        .canonicalize()
        .map_err(|_| refused("workspace is unavailable"))?;
    for selector in &task.context_files {
        let anchor = anchor_path(selector).map_err(|error| refused(error.to_string()))?;
        let resolved = canonical_workspace
            .join(anchor)
            .canonicalize()
            .map_err(|_| refused("scope anchor is unavailable; reconcile the task selectors"))?;
        if !resolved.starts_with(&canonical_workspace) {
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
pub(super) fn verify_handoff<H: RuntimeHost + ?Sized>(
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
    compare_checkpoint(checkpoint, &checked)
}

/// Recheck a clean-tree checkpoint against the owner-observed base revision.
pub(super) fn verify_handoff_at_revision<H: RuntimeHost + ?Sized>(
    host: &H,
    task: &Task,
    workspace: &Path,
    run_id: &str,
    checkpoint: &Value,
) -> Result<(), OrbitError> {
    let head = checkpoint["base_sha"].as_str().unwrap_or_default();
    let checked = verify_at_revision(host, task, workspace, run_id, head)?;
    compare_checkpoint(checkpoint, &checked)
}

fn compare_checkpoint(checkpoint: &Value, checked: &Value) -> Result<(), OrbitError> {
    if checkpoint["decision"] != DECISION
        || checkpoint["already_landed"] != checked["already_landed"]
        || checkpoint["validation_provenance"] != checked["validation_provenance"]
    {
        return Err(refused(
            "accepted landing evidence changed; rerun the commit verification step",
        ));
    }
    Ok(())
}

/// Whether `evidence_run` is the current run or a run in its recorded retry
/// lineage. Shared with the no-diff verifier.
pub(super) fn run_in_lineage<H: RuntimeHost + ?Sized>(
    host: &H,
    run_id: &str,
    evidence_run: &str,
) -> Result<bool, OrbitError> {
    let mut current = run_id.to_string();
    let mut seen = BTreeSet::new();
    for _ in 0..64 {
        if !evidence_run.is_empty() && current == evidence_run {
            return Ok(true);
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
    Ok(false)
}

fn verify_checks<H: RuntimeHost + ?Sized>(
    host: &H,
    evidence: &Evidence,
    task: &Task,
) -> Result<Vec<Value>, OrbitError> {
    let commands: BTreeSet<_> = evidence.required_commands.iter().collect();
    if commands.is_empty()
        || commands.len() != evidence.required_commands.len()
        || commands.iter().any(|command| command.trim().is_empty())
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
        match validation.role {
            ValidationRole::Required => {
                if validation.outcome != ValidationOutcome::Passed
                    || !commands.contains(&validation.command)
                    || !checked.insert(&validation.command)
                {
                    return Err(refused(
                        "required validation is missing, duplicated, failed, denied or not run",
                    ));
                }
            }
            ValidationRole::ExpectedFailure | ValidationRole::Diagnostic => {}
            ValidationRole::Excluded | ValidationRole::Superseded => {
                return Err(refused(
                    "already-landed validation accepts only required, expected_failure or diagnostic checks",
                ));
            }
        }
        let log = artifact(host, &task.id, &check.log_artifact)?;
        let provenance: Value = serde_json::from_slice(&log.content).map_err(|error| {
            refused(format!(
                "invalid validation log {}: {error}",
                check.log_artifact
            ))
        })?;
        let exit_code = provenance["exit_code"].as_i64();
        let exit_matches = match validation.role {
            ValidationRole::Required => exit_code == Some(0),
            ValidationRole::ExpectedFailure => exit_code.is_some_and(|code| code != 0),
            ValidationRole::Diagnostic => exit_code.is_some(),
            ValidationRole::Excluded | ValidationRole::Superseded => false,
        };
        if validation.command.trim().is_empty()
            || !exit_matches
            || !log_has_provenance(
                &provenance,
                &evidence.run_id,
                &evidence.tested_head,
                &validation.command,
            )
        {
            return Err(refused(
                "validation log must capture the exact run, tested HEAD, command, integer exit code and output; required checks must exit zero and expected_failure checks must exit nonzero",
            ));
        }
        logs.push(provenance);
    }
    if checked != commands {
        return Err(refused("required validation is missing"));
    }
    Ok(logs)
}

/// A captured validation log names the exact run, tested HEAD and command,
/// with a zero exit code and captured (possibly empty) output.
pub(super) fn log_matches(
    provenance: &Value,
    run_id: &str,
    tested_head: &str,
    command: &str,
) -> bool {
    log_has_provenance(provenance, run_id, tested_head, command) && provenance["exit_code"] == 0
}

fn log_has_provenance(provenance: &Value, run_id: &str, tested_head: &str, command: &str) -> bool {
    provenance["run_id"] == run_id
        && provenance["tested_head"] == tested_head
        && provenance["command"] == command
        && provenance["output"].as_str().is_some()
}

fn artifact<H: RuntimeHost + ?Sized>(
    host: &H,
    task_id: &str,
    path: &str,
) -> Result<TaskArtifact, OrbitError> {
    host.get_task_artifact(task_id, path)?
        .ok_or_else(|| refused(format!("missing task artifact {path}; attach structured landing proof and required validation logs")))
}

fn refused(reason: impl std::fmt::Display) -> OrbitError {
    OrbitError::Execution(format!("already_landed_unverified: {reason}"))
}
