//! Shared clean-tree guards and checkpoint rechecks for structured evidence.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_types::task::{Task, TaskArtifact};
use serde_json::Value;

use crate::context::RuntimeHost;

use super::super::git::git_output;

/// The evidence kind's existing checkpoint contract and refusal wording.
pub(super) struct Specification {
    pub(super) decision: &'static str,
    pub(super) evidence_key: &'static str,
    pub(super) refusal_prefix: &'static str,
    pub(super) head_changed: &'static str,
    pub(super) dirty_tree: &'static str,
    pub(super) missing_artifact_suffix: &'static str,
    pub(super) task_count: &'static str,
    pub(super) evidence_changed: &'static str,
}

/// Evidence-specific checks surrounded by the same commit and handoff guards.
pub(super) trait Verifier {
    const SPEC: Specification;

    /// Recheck Git state before parsing evidence, preserving refusal order.
    fn verify<H: RuntimeHost + ?Sized>(
        host: &H,
        task: &Task,
        workspace: &Path,
        run_id: &str,
        tested_head: &str,
    ) -> Result<Value, OrbitError> {
        if git_output(workspace, &["rev-parse", "HEAD"])? != tested_head {
            return Err(Self::refused(Self::SPEC.head_changed));
        }
        if !git_output(
            workspace,
            &["status", "--porcelain", "--untracked-files=all"],
        )?
        .is_empty()
        {
            return Err(Self::refused(Self::SPEC.dirty_tree));
        }
        Self::check_artifacts(host, task)?;
        Self::verify_at_revision(host, task, workspace, run_id, tested_head)
    }

    /// Optional guidance before evidence parsing, after the clean-tree check.
    fn check_artifacts<H: RuntimeHost + ?Sized>(_host: &H, _task: &Task) -> Result<(), OrbitError> {
        Ok(())
    }

    /// Verify evidence against immutable Git objects and task artifacts.
    fn verify_at_revision<H: RuntimeHost + ?Sized>(
        host: &H,
        task: &Task,
        workspace: &Path,
        run_id: &str,
        tested_head: &str,
    ) -> Result<Value, OrbitError>;

    /// Promotion and completion require the exact evidence accepted at commit.
    fn verify_handoff<H: RuntimeHost + ?Sized>(
        host: &H,
        tasks: &[Task],
        workspace: &Path,
        run_id: &str,
        checkpoint: &Value,
    ) -> Result<(), OrbitError> {
        let [task] = tasks else {
            return Err(Self::refused(Self::SPEC.task_count));
        };
        let head = checkpoint["base_sha"].as_str().unwrap_or_default();
        let checked = Self::verify(host, task, workspace, run_id, head)?;
        Self::compare_checkpoint(checkpoint, &checked)
    }

    /// Recheck against the owner's observed base without changing its checkout.
    fn verify_handoff_at_revision<H: RuntimeHost + ?Sized>(
        host: &H,
        task: &Task,
        workspace: &Path,
        run_id: &str,
        checkpoint: &Value,
    ) -> Result<(), OrbitError> {
        let head = checkpoint["base_sha"].as_str().unwrap_or_default();
        let checked = Self::verify_at_revision(host, task, workspace, run_id, head)?;
        Self::compare_checkpoint(checkpoint, &checked)
    }

    fn compare_checkpoint(checkpoint: &Value, checked: &Value) -> Result<(), OrbitError> {
        if checkpoint["decision"] != Self::SPEC.decision
            || checkpoint[Self::SPEC.evidence_key] != checked[Self::SPEC.evidence_key]
            || checkpoint["validation_provenance"] != checked["validation_provenance"]
        {
            return Err(Self::refused(Self::SPEC.evidence_changed));
        }
        Ok(())
    }

    fn artifact<H: RuntimeHost + ?Sized>(
        host: &H,
        task_id: &str,
        path: &str,
    ) -> Result<TaskArtifact, OrbitError> {
        host.get_task_artifact(task_id, path)?.ok_or_else(|| {
            Self::refused(format!(
                "missing task artifact {path}{}",
                Self::SPEC.missing_artifact_suffix
            ))
        })
    }

    fn refused(reason: impl std::fmt::Display) -> OrbitError {
        OrbitError::Execution(format!("{}: {reason}", Self::SPEC.refusal_prefix))
    }
}
