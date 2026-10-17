//! Revalidation of a run's structured claim that its implementation correctly
//! changed nothing [ORB-13145]. The evidence names this task, this run (or its
//! retry lineage), and the pinned HEAD it validated, plus zero-exit validation
//! with captured logs; Git state must still agree before the commit step may
//! finish clean without inventing a commit or pointing at unrelated history.

use std::collections::BTreeSet;
use std::path::Path;

use orbit_common::OrbitError;
use orbit_types::task::Task;
use orbit_types::workflow::handoff::NoDiffEvidence;
use serde_json::{Value, json};

use crate::context::RuntimeHost;

use super::already_landed::{log_matches, run_in_lineage};
use super::checkpoint::pinned_object_id;
use super::clean_tree::{Specification, Verifier};

pub(super) const ARTIFACT: &str = "no-diff.json";

/// Checkpoint decision for a verified no-diff implementation.
pub(super) const DECISION: &str = "verified_no_diff";

pub(super) struct NoDiff;

impl Verifier for NoDiff {
    const SPEC: Specification = Specification {
        decision: DECISION,
        evidence_key: "no_diff",
        refusal_prefix: "no_diff_unverified",
        head_changed: "tested HEAD is not the pinned current HEAD; rerun validation on it",
        dirty_tree: "worktree is not clean; deliver or reconcile the pending changes",
        missing_artifact_suffix: "",
        task_count: "no-diff completion requires exactly one task",
        evidence_changed: "accepted no-diff evidence changed; rerun the commit verification step",
    };

    fn verify_at_revision<H: RuntimeHost + ?Sized>(
        host: &H,
        task: &Task,
        _workspace: &Path,
        run_id: &str,
        tested_head: &str,
    ) -> Result<Value, OrbitError> {
        let report = Self::artifact(host, &task.id, ARTIFACT)?;
        let evidence: NoDiffEvidence = serde_json::from_slice(&report.content)
            .map_err(|error| Self::refused(format!("invalid {ARTIFACT}: {error}")))?;
        if evidence.schema_version != 1 || evidence.task_id != task.id {
            return Err(Self::refused("evidence must identify this task"));
        }
        if evidence.reason.trim().is_empty() {
            return Err(Self::refused(
                "evidence must state why no change was required",
            ));
        }
        if !run_in_lineage(host, run_id, &evidence.run_id)? {
            return Err(Self::refused(
                "evidence belongs to a different run; validate in this run or its recorded retry lineage",
            ));
        }
        let head = pinned_object_id(&evidence.tested_head)?;
        if head != tested_head {
            return Err(Self::refused("tested HEAD is not the owner-observed base"));
        }
        let validation_provenance = verify_checks(host, &task.id, &evidence)?;

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
}

fn verify_checks<H: RuntimeHost + ?Sized>(
    host: &H,
    task_id: &str,
    evidence: &NoDiffEvidence,
) -> Result<Vec<Value>, OrbitError> {
    if evidence.validation.is_empty() {
        return Err(NoDiff::refused("list the validation this run executed"));
    }
    let mut commands = BTreeSet::new();
    let mut logs = Vec::new();
    for check in &evidence.validation {
        if check.command.trim().is_empty() || !commands.insert(&check.command) {
            return Err(NoDiff::refused(
                "validation commands must be non-empty and unique",
            ));
        }
        if check.exit_code != 0 {
            return Err(NoDiff::refused(format!(
                "validation '{}' exited {}",
                check.command, check.exit_code
            )));
        }
        let log = NoDiff::artifact(host, task_id, &check.log_artifact)?;
        let provenance: Value = serde_json::from_slice(&log.content).map_err(|error| {
            NoDiff::refused(format!(
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
            return Err(NoDiff::refused(
                "validation log must capture the exact run, tested HEAD, command, zero exit code and output",
            ));
        }
        logs.push(provenance);
    }
    Ok(logs)
}
