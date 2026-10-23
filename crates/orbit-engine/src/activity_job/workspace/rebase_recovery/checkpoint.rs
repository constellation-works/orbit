use std::fs;
use std::path::Path;

use serde_json::Value;

use super::super::fingerprint::git_stdout;
use super::super::{DispatchError, WorktreeBoundaryGuard};
use super::metadata::RecoveryMetadata;

pub(in crate::activity_job::workspace) struct RebaseRecoveryCheckpoint {
    pub(super) metadata: RecoveryMetadata,
    /// The failed step this recovery completes.
    pub(super) step_id: String,
    /// The attempt the host reserved when it admitted this recovery. Held only
    /// here, never read back from provider output or the run store.
    pub(super) attempt: u64,
    pub(super) branch: String,
    pub(super) original_head: String,
    pub(super) original_base_sha: String,
    pub(super) base_ref: String,
    pub(super) target_base_sha: String,
    pub(super) conflicting_paths: Vec<String>,
    pub(super) remote_sha_before: Option<String>,
    /// The task's scope when recovery was admitted; an absorbed continuation
    /// carries it so settlement can refuse a task edited since [ORB-14668].
    pub(super) task_scope_digest: Option<String>,
}

/// How the host continuation of an admitted stopped rebase ended.
pub(in crate::activity_job::workspace) enum RebaseContinuation {
    /// The rebase finished on the pinned base (and followed an advanced
    /// tip when it could); the payload is the checkpoint the host certifies.
    Completed(Value),
    /// `rebase --continue` committed the repaired pick, then stopped on a
    /// later commit of the same pinned rebase with new unmerged paths. The
    /// progress is kept: the executor's retry reports the new stop as a
    /// typed conflict and the next bounded recovery round resolves it.
    StoppedAgain,
    /// The rebase finished exactly on the pinned base, every pick dropped as
    /// empty, and a commit on that base covers the candidate's change. The
    /// payload is the checkpoint the host certifies, with its `absorbed`
    /// evidence; the retry hands it to verified no-diff settlement
    /// [ORB-14668].
    Absorbed(Value),
}

impl WorktreeBoundaryGuard {
    pub(super) fn validate_rebase_checkpoint(
        &self,
        checkpoint: &RebaseRecoveryCheckpoint,
        invalid: &impl Fn(&str) -> DispatchError,
    ) -> Result<(), DispatchError> {
        let expected_ref = format!("refs/heads/{}", checkpoint.branch);
        // `base_ref` may have advanced past the pin while the provider ran;
        // the stopped rebase's own `onto` below is the pin that matters.
        if git_stdout(
            &self.assigned_root,
            &["rev-parse", "--verify", &expected_ref],
        )? != checkpoint.original_head
            || git_stdout(
                &self.assigned_root,
                &[
                    "merge-base",
                    &checkpoint.original_head,
                    &checkpoint.target_base_sha,
                ],
            )? != checkpoint.original_base_sha
        {
            return Err(invalid("branch, original HEAD, or original base moved"));
        }
        for backend in ["rebase-merge", "rebase-apply"] {
            let path = git_stdout(
                &self.assigned_root,
                &["rev-parse", "--path-format=absolute", "--git-path", backend],
            )?;
            let path = Path::new(&path);
            if !path.is_dir() {
                continue;
            }
            let read = |name: &str| {
                fs::read_to_string(path.join(name)).map(|text| text.trim().to_string())
            };
            if read("head-name").ok().as_deref() == Some(expected_ref.as_str())
                && read("orig-head").ok().as_deref() == Some(checkpoint.original_head.as_str())
                && read("onto").ok().as_deref() == Some(checkpoint.target_base_sha.as_str())
            {
                return Ok(());
            }
        }
        Err(invalid(
            "the stopped rebase metadata no longer matches its checkpoint",
        ))
    }
}
