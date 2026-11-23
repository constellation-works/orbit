//! Shared clean-tree guards and checkpoint rechecks for structured evidence.

use std::collections::BTreeSet;
use std::path::Path;

use orbit_common::OrbitError;
use orbit_types::task::{Task, TaskArtifact};
use serde_json::Value;

use crate::context::RuntimeHost;

use super::super::git::{git_output, git_output_paths};

/// Remove untracked, ignored-by-convention Python interpreter byproducts
/// (`__pycache__/`, `*.pyc`) directly from `workspace` (never through Trash).
///
/// Tracked files (including modified `.pyc` fixtures), staged entries,
/// symlinks, `.git`/`.orbit` paths, and non-byproduct untracked files are left
/// untouched.
pub(in crate::executor::automation::vcs) fn remove_untracked_interpreter_byproducts(
    workspace: &Path,
) -> Result<Vec<String>, OrbitError> {
    let untracked = git_output_paths(
        workspace,
        &["ls-files", "--others", "--exclude-standard", "-z", "--"],
    )?;
    let mut removed = Vec::new();
    let mut candidate_dirs = BTreeSet::new();
    for rel in untracked {
        if !is_untracked_interpreter_byproduct(&rel)
            || !untracked_regular_file_without_symlinks(workspace, &rel)
        {
            continue;
        }
        let abs = workspace.join(&rel);
        match std::fs::remove_file(&abs) {
            Ok(()) => {
                removed.push(rel.clone());
                collect_pycache_ancestors(&rel, &mut candidate_dirs);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(OrbitError::Execution(format!(
                    "remove untracked interpreter byproduct '{}': {error}",
                    abs.display()
                )));
            }
        }
    }
    // Prune now-empty `__pycache__` directories deepest-first without
    // following symlinks or deleting directories that hold other files.
    let mut dirs: Vec<String> = candidate_dirs.into_iter().collect();
    dirs.sort_by(|left, right| right.len().cmp(&left.len()).then_with(|| right.cmp(left)));
    for rel_dir in dirs {
        if !real_directory_without_symlinks(workspace, &rel_dir) {
            continue;
        }
        let abs_dir = workspace.join(&rel_dir);
        match std::fs::remove_dir(&abs_dir) {
            Ok(()) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
                ) => {}
            Err(error) => {
                return Err(OrbitError::Execution(format!(
                    "remove empty interpreter cache directory '{}': {error}",
                    abs_dir.display()
                )));
            }
        }
    }
    Ok(removed)
}

fn is_untracked_interpreter_byproduct(rel: &str) -> bool {
    let mut parts = rel.split('/').peekable();
    let mut in_pycache = false;
    while let Some(part) = parts.next() {
        if part.is_empty()
            || part == "."
            || part == ".."
            || part.eq_ignore_ascii_case(".git")
            || part == ".orbit"
        {
            return false;
        }
        if parts.peek().is_some() {
            if part == "__pycache__" {
                in_pycache = true;
            }
        } else {
            return in_pycache || part.ends_with(".pyc");
        }
    }
    false
}

fn collect_pycache_ancestors(rel: &str, dirs: &mut BTreeSet<String>) {
    let mut prefix = String::new();
    let mut under_pycache = false;
    let mut parts = rel.split('/').peekable();
    while let Some(part) = parts.next() {
        if parts.peek().is_none() {
            break;
        }
        if !prefix.is_empty() {
            prefix.push('/');
        }
        prefix.push_str(part);
        if part == "__pycache__" {
            under_pycache = true;
        }
        if under_pycache {
            dirs.insert(prefix.clone());
        }
    }
}

fn untracked_regular_file_without_symlinks(workspace: &Path, rel: &str) -> bool {
    let mut current = workspace.to_path_buf();
    let mut parts = rel.split('/').peekable();
    while let Some(part) = parts.next() {
        current.push(part);
        let Ok(metadata) = std::fs::symlink_metadata(&current) else {
            return false;
        };
        let kind = metadata.file_type();
        if parts.peek().is_some() {
            if !kind.is_dir() {
                return false;
            }
        } else {
            return kind.is_file();
        }
    }
    false
}

fn real_directory_without_symlinks(workspace: &Path, rel_dir: &str) -> bool {
    let mut current = workspace.to_path_buf();
    for part in rel_dir.split('/') {
        current.push(part);
        if !std::fs::symlink_metadata(&current).is_ok_and(|metadata| metadata.file_type().is_dir())
        {
            return false;
        }
    }
    true
}

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
        Self::verify_clean_checkout(workspace, tested_head)?;
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

    /// [`Self::verify_at_revision`] allowing the owner's recording of the
    /// `regression_from` relations the handoff itself supplied [ORB-24696].
    /// Only the already-landed verifier's scope comparison uses them.
    fn verify_at_revision_with_supplied_regression_from<H: RuntimeHost + ?Sized>(
        host: &H,
        task: &Task,
        workspace: &Path,
        run_id: &str,
        tested_head: &str,
        _supplied_regression_from: &[String],
    ) -> Result<Value, OrbitError> {
        Self::verify_at_revision(host, task, workspace, run_id, tested_head)
    }

    /// [`Self::verify`] through
    /// [`Self::verify_at_revision_with_supplied_regression_from`].
    fn verify_with_supplied_regression_from<H: RuntimeHost + ?Sized>(
        host: &H,
        task: &Task,
        workspace: &Path,
        run_id: &str,
        tested_head: &str,
        supplied_regression_from: &[String],
    ) -> Result<Value, OrbitError> {
        Self::verify_clean_checkout(workspace, tested_head)?;
        Self::check_artifacts(host, task)?;
        Self::verify_at_revision_with_supplied_regression_from(
            host,
            task,
            workspace,
            run_id,
            tested_head,
            supplied_regression_from,
        )
    }

    fn verify_clean_checkout(workspace: &Path, tested_head: &str) -> Result<(), OrbitError> {
        if git_output(workspace, &["rev-parse", "HEAD"])? != tested_head {
            return Err(Self::refused(Self::SPEC.head_changed));
        }
        remove_untracked_interpreter_byproducts(workspace)?;
        if !git_output(
            workspace,
            &["status", "--porcelain", "--untracked-files=all"],
        )?
        .is_empty()
        {
            return Err(Self::refused(Self::SPEC.dirty_tree));
        }
        Ok(())
    }

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
        Self::verify_handoff_at_revision_with_supplied_regression_from(
            host,
            task,
            workspace,
            run_id,
            checkpoint,
            &[],
        )
    }

    /// [`Self::verify_handoff_at_revision`] allowing the owner's recording of
    /// the `regression_from` relations the handoff itself supplied.
    fn verify_handoff_at_revision_with_supplied_regression_from<H: RuntimeHost + ?Sized>(
        host: &H,
        task: &Task,
        workspace: &Path,
        run_id: &str,
        checkpoint: &Value,
        supplied_regression_from: &[String],
    ) -> Result<(), OrbitError> {
        let head = checkpoint["base_sha"].as_str().unwrap_or_default();
        let checked = Self::verify_at_revision_with_supplied_regression_from(
            host,
            task,
            workspace,
            run_id,
            head,
            supplied_regression_from,
        )?;
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
