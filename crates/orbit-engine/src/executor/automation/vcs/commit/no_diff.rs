//! Revalidation of a run's structured claim that its implementation correctly
//! changed nothing [ORB-13145]. The evidence names this task, this run (or its
//! retry lineage), and the pinned HEAD it validated, plus zero-exit validation
//! with captured logs; Git state must still agree before the commit step may
//! finish clean without inventing a commit or pointing at unrelated history.

use std::collections::BTreeSet;
use std::path::Path;

use orbit_common::OrbitError;
use orbit_types::task::{Task, TaskArtifact};
use orbit_types::workflow::handoff::NoDiffEvidence;
use serde_json::{Value, json};

use crate::context::RuntimeHost;

use super::super::git::git_output;
use super::already_landed::{log_matches, run_in_lineage};
use super::pinned_object_id;

pub(super) const ARTIFACT: &str = "no-diff.json";

/// Checkpoint decision for a verified no-diff implementation.
pub(super) const DECISION: &str = "verified_no_diff";

/// Recheck the evidence at commit, promotion, and completion. The checkpoint
/// carries the accepted evidence and logs so later steps can detect tampering.
pub(super) fn verify<H: RuntimeHost + ?Sized>(
    host: &H,
    task: &Task,
    workspace: &Path,
    run_id: &str,
    tested_head: &str,
) -> Result<Value, OrbitError> {
    let artifacts = host.get_task_artifacts(&task.id)?;
    let report = artifact(&artifacts, ARTIFACT)?;
    let evidence: NoDiffEvidence = serde_json::from_slice(&report.content)
        .map_err(|error| refused(format!("invalid {ARTIFACT}: {error}")))?;
    if evidence.schema_version != 1 || evidence.task_id != task.id {
        return Err(refused("evidence must identify this task"));
    }
    if evidence.reason.trim().is_empty() {
        return Err(refused("evidence must state why no change was required"));
    }
    if !run_in_lineage(host, run_id, &evidence.run_id)? {
        return Err(refused(
            "evidence belongs to a different run; validate in this run or its recorded retry lineage",
        ));
    }
    let head = pinned_object_id(&evidence.tested_head)?;
    if head != tested_head || git_output(workspace, &["rev-parse", "HEAD"])? != head {
        return Err(refused(
            "tested HEAD is not the pinned current HEAD; rerun validation on it",
        ));
    }
    if !git_output(
        workspace,
        &["status", "--porcelain", "--untracked-files=all"],
    )?
    .is_empty()
    {
        return Err(refused(
            "worktree is not clean; deliver or reconcile the pending changes",
        ));
    }
    let validation_provenance = verify_checks(&evidence, &artifacts)?;

    Ok(json!({
        "phase": "commit",
        "decision": DECISION,
        "committed": false,
        "skipped_no_diff_expected": true,
        "task_id": task.id,
        "job_run_id": run_id,
        "base_sha": head,
        "no_diff": evidence,
        "validation_provenance": validation_provenance,
    }))
}

/// Promotion and completion recheck the live evidence and require it to match
/// the checkpoint the commit step accepted.
pub(super) fn verify_handoff<H: RuntimeHost + ?Sized>(
    host: &H,
    tasks: &[Task],
    workspace: &Path,
    run_id: &str,
    checkpoint: &Value,
) -> Result<(), OrbitError> {
    let [task] = tasks else {
        return Err(refused("no-diff completion requires exactly one task"));
    };
    let head = checkpoint["base_sha"].as_str().unwrap_or_default();
    let checked = verify(host, task, workspace, run_id, head)?;
    if checkpoint["decision"] != DECISION
        || checkpoint["no_diff"] != checked["no_diff"]
        || checkpoint["validation_provenance"] != checked["validation_provenance"]
    {
        return Err(refused(
            "accepted no-diff evidence changed; rerun the commit verification step",
        ));
    }
    Ok(())
}

fn verify_checks(
    evidence: &NoDiffEvidence,
    artifacts: &[TaskArtifact],
) -> Result<Vec<Value>, OrbitError> {
    if evidence.validation.is_empty() {
        return Err(refused("list the validation this run executed"));
    }
    let mut commands = BTreeSet::new();
    let mut logs = Vec::new();
    for check in &evidence.validation {
        if check.command.trim().is_empty() || !commands.insert(&check.command) {
            return Err(refused("validation commands must be non-empty and unique"));
        }
        if check.exit_code != 0 {
            return Err(refused(format!(
                "validation '{}' exited {}",
                check.command, check.exit_code
            )));
        }
        let log = artifact(artifacts, &check.log_artifact)?;
        let provenance: Value = serde_json::from_slice(&log.content).map_err(|error| {
            refused(format!(
                "invalid validation log {}: {error}",
                check.log_artifact
            ))
        })?;
        if !log_matches(
            &provenance,
            &evidence.run_id,
            &evidence.tested_head,
            &check.command,
        ) {
            return Err(refused(
                "validation log must capture the exact run, tested HEAD, command, zero exit code and output",
            ));
        }
        logs.push(provenance);
    }
    Ok(logs)
}

fn artifact<'a>(artifacts: &'a [TaskArtifact], path: &str) -> Result<&'a TaskArtifact, OrbitError> {
    artifacts
        .iter()
        .find(|artifact| artifact.path == path)
        .ok_or_else(|| refused(format!("missing task artifact {path}")))
}

fn refused(reason: impl std::fmt::Display) -> OrbitError {
    OrbitError::Execution(format!("no_diff_unverified: {reason}"))
}
