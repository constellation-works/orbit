//! [ORB-14268] What changed around a failed step while it was recovered.
//!
//! A `step_failure_recovery` invocation that writes no decision has asserted
//! nothing. Its post-recovery attempt is admitted only when the engine sees
//! that something the failure depended on changed since the failure: the
//! assigned worktree (HEAD, index, tracked and untracked content), the tip of
//! the run's base ref, or the environment required validation runs with.
//! Otherwise the rerun would meet the cause that just failed it.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::*;
use crate::activity_job::workspace::fingerprint::git_fingerprint;
use crate::executor::automation::vcs::environment_record;
use crate::executor::automation::vcs::git::git_output;

/// The failed step's surroundings as observed before recovery ran.
pub(super) struct RecoveryObservation {
    workspace_root: PathBuf,
    base_ref: Option<String>,
    state: ObservedState,
}

/// A component the engine could not observe is `None` and never counts as a
/// change.
struct ObservedState {
    worktree: Option<String>,
    base: Option<String>,
    validation_env: Value,
}

impl RecoveryObservation {
    /// Observe `workspace_root` and the tip of `base_ref` in it now.
    pub(super) fn take(
        ctx: &ExecCtx<'_>,
        workspace_root: PathBuf,
        base_ref: Option<String>,
    ) -> Self {
        let state = ObservedState::take(ctx, &workspace_root, base_ref.as_deref());
        Self {
            workspace_root,
            base_ref,
            state,
        }
    }

    /// Observe again and name what differs from the first observation;
    /// empty when nothing observable changed.
    pub(super) fn changes(&self, ctx: &ExecCtx<'_>) -> Vec<&'static str> {
        let later = ObservedState::take(ctx, &self.workspace_root, self.base_ref.as_deref());
        let mut changes = Vec::new();
        if changed(&self.state.worktree, &later.worktree) {
            changes.push("worktree");
        }
        if changed(&self.state.base, &later.base) {
            changes.push("base");
        }
        if self.state.validation_env != later.validation_env {
            changes.push("validation environment");
        }
        changes
    }
}

impl ObservedState {
    fn take(ctx: &ExecCtx<'_>, workspace_root: &Path, base_ref: Option<&str>) -> Self {
        let worktree = git_fingerprint(workspace_root)
            .ok()
            .and_then(|fingerprint| serde_json::to_vec(&fingerprint).ok())
            .map(|bytes| hex_sha256(&bytes));
        let base = base_ref.and_then(|base_ref| {
            git_output(
                workspace_root,
                &["rev-parse", "--verify", &format!("{base_ref}^{{commit}}")],
            )
            .ok()
        });
        Self {
            worktree,
            base,
            validation_env: environment_record(&ctx.host.validation_subprocess_environment()),
        }
    }
}

/// The base ref the failed step works against: its own rendered input's
/// `base_ref`, else the first completed step output (by step id) that names
/// one for the same worktree.
pub(super) fn recovery_base_ref(ctx: &ExecCtx<'_>, input: &Value) -> Option<String> {
    let text = |value: &Value, key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string)
    };
    if let Some(base_ref) = input
        .get("failed_step_input")
        .and_then(|failed| text(failed, "base_ref"))
    {
        return Some(base_ref);
    }
    let workspace = text(input, "workspace_path");
    let pipeline = ctx.pipeline_snapshot();
    let mut steps: Vec<_> = pipeline.iter().collect();
    steps.sort_by_key(|(step_id, _)| *step_id);
    steps.into_iter().find_map(|(_, wrapped)| {
        let output = wrapped.get("output")?;
        if workspace.is_some() && text(output, "workspace_path") != workspace {
            return None;
        }
        text(output, "base_ref")
    })
}

/// Both sides observed and different.
fn changed(before: &Option<String>, after: &Option<String>) -> bool {
    matches!((before, after), (Some(before), Some(after)) if before != after)
}

fn hex_sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
