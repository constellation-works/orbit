//! An implementer that found its pinned base already resolves the task,
//! without attaching a clean-tree report [ORB-22407].
//!
//! The artifact-backed routes (`no-diff.json`, `already-landed.json`) stay
//! the authority when an implementer attaches one. A claimed task whose
//! implementer instead carried its evidence in its output — admitted
//! `already_resolved` test selectors, with its summary beside them —
//! settles through this checkpoint instead of failing the commit step with
//! an empty stage, whose only triage answer requeued the same empty run.
//! The checkpoint embeds that evidence — the admitted selectors, plus the
//! implementer's summary when it gave one — so an operator can settle the
//! task from the outcome alone, and claimed NoDiff validation rechecks it on
//! the clean base while running the required commands and replaying the
//! named selectors there. Owner runs instead attach a verifier report and
//! validation logs; they have no claim-host selector replay [ORB-23194].

use std::path::Path;

use orbit_common::OrbitError;
use orbit_types::task::Task;
use orbit_types::workflow::handoff::AlreadyResolved;
use serde_json::{Value, json};

use super::super::conventions::meaningful_execution_summary;
use super::super::git::git_output;
use super::super::handoff::reports_failure;

/// Checkpoint decision for an already-resolved clean base.
pub(super) const DECISION: &str = "verified_already_resolved";

const EVIDENCE_KEY: &str = "already_resolved_evidence";

/// The implementer's evidence in its pipeline output, when it can stand in
/// for a clean-tree report: admitted `already_resolved` selectors naming
/// the tests that show the base resolves the task, with the implementer's
/// meaningful summary carried beside them when it gave one. A summary
/// alone names no check that could show the base resolves the task, so it
/// is not evidence here. Invalid selectors refuse rather than settle.
pub(super) fn evidence_from_implementation(input: &Value) -> Result<Option<Value>, OrbitError> {
    let Some(implementation) = input
        .get("implementation")
        .filter(|value| value.is_object())
    else {
        return Ok(None);
    };
    let selectors = match AlreadyResolved::from_output(implementation) {
        Ok(Some(declared)) => {
            declared.admit().map_err(OrbitError::InvalidInput)?;
            declared.selectors
        }
        Ok(None) => Vec::new(),
        Err(error) => return Err(OrbitError::InvalidInput(error)),
    };
    let summary = super::super::claim::implementer_summary(input)
        .filter(|summary| meaningful_execution_summary(summary).is_some())
        .filter(|summary| !reports_failure(summary));
    if selectors.is_empty() {
        return Ok(None);
    }
    Ok(Some(json!({
        "execution_summary": summary,
        "selectors": selectors,
    })))
}

/// The settled commit outcome for an already-resolved task. It reuses the
/// NoDiff route (`skipped_no_diff_expected`), so a claimed pipeline hands off
/// without publishing a branch or pull request after claim-host validation.
pub(super) fn checkpoint(task_id: &str, run_id: &str, base_sha: &str, evidence: Value) -> Value {
    json!({
        "phase": "commit",
        "decision": DECISION,
        "committed": false,
        "skipped_no_diff_expected": true,
        "task_id": task_id,
        "job_run_id": run_id,
        "base_sha": base_sha,
        EVIDENCE_KEY: evidence,
    })
}

/// Recheck the checkpoint on the live worktree: HEAD is the pinned
/// base and nothing is pending.
pub(super) fn verify_handoff(
    task: &Task,
    workspace: &Path,
    checkpoint: &Value,
) -> Result<(), OrbitError> {
    verify_checkpoint(task, checkpoint)?;
    if git_output(workspace, &["rev-parse", "HEAD"])?
        != checkpoint["base_sha"].as_str().unwrap_or_default()
    {
        return Err(refused("HEAD is not the checkpoint's pinned base"));
    }
    if !git_output(
        workspace,
        &["status", "--porcelain", "--untracked-files=all"],
    )?
    .is_empty()
    {
        return Err(refused("worktree is not clean"));
    }
    Ok(())
}

/// The owner's recheck against its observed base: the checkpoint names this
/// task and its pinned base, and its embedded evidence still admits.
pub(super) fn verify_handoff_at_revision(
    task: &Task,
    checkpoint: &Value,
) -> Result<(), OrbitError> {
    verify_checkpoint(task, checkpoint)
}

fn verify_checkpoint(task: &Task, checkpoint: &Value) -> Result<(), OrbitError> {
    if checkpoint["decision"] != DECISION || checkpoint["task_id"] != task.id.as_str() {
        return Err(refused("checkpoint must name this task"));
    }
    if checkpoint["base_sha"].as_str().is_none_or(str::is_empty) {
        return Err(refused("checkpoint must name its pinned base"));
    }
    let evidence = &checkpoint[EVIDENCE_KEY];
    let selectors = evidence["selectors"].as_array().map(|selectors| {
        selectors
            .iter()
            .filter_map(Value::as_str)
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>()
    });
    match selectors {
        Some(selectors) if !selectors.is_empty() => {
            AlreadyResolved { selectors }
                .admit()
                .map_err(|error| refused(format!("evidence selectors do not admit: {error}")))?;
        }
        _ => {
            return Err(refused(
                "checkpoint must carry the implementer's already-resolved selectors",
            ));
        }
    }
    Ok(())
}

fn refused(reason: impl std::fmt::Display) -> OrbitError {
    OrbitError::PolicyDenied(format!("already_resolved_unverified: {reason}"))
}
