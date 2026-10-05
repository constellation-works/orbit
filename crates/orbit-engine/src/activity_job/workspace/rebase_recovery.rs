use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};

use orbit_types::task::ContextWideningStep;

use crate::context::{RebaseRecoveryAttemptScope, RuntimeHost, StepRecoveryAdmission};
use crate::executor::automation::vcs::git::{GitBytesOutcome, git_run_bytes};

use super::boundary_guard::is_host_owned_path;
use super::fingerprint::{
    GitWorktreeFingerprint, changed_paths, git_command_error, git_fingerprint, git_output_raw,
    git_stdout, git_stdout_bytes, nul_paths,
};
use super::recovery::safe_relative_path;
use super::{DispatchError, WorktreeBoundaryGuard};

pub(super) struct RebaseRecoveryCheckpoint {
    metadata: RecoveryMetadata,
    /// The failed step this recovery completes.
    step_id: String,
    /// The attempt the host reserved when it admitted this recovery. Held only
    /// here, never read back from provider output or the run store.
    attempt: u64,
    branch: String,
    original_head: String,
    original_base_sha: String,
    base_ref: String,
    target_base_sha: String,
    conflicting_paths: Vec<String>,
    remote_sha_before: Option<String>,
}

impl WorktreeBoundaryGuard {
    /// Admit file repair for the already stopped, checkpoint-matching rebase.
    /// The provider remains unable to write Git metadata; after it exits, the
    /// host boundary independently validates and completes this checkpoint.
    /// Admission reserves the host's attempt identity for this recovery, so a
    /// later legitimate recovery of the same step certifies as a new attempt
    /// instead of colliding with this one.
    pub(crate) fn authorize_rebase_completion(
        &mut self,
        host: &dyn RuntimeHost,
        input: &Value,
    ) -> Result<(), DispatchError> {
        let invalid = || {
            DispatchError::CliInvocationPermanent(
                "conflict recovery requires an existing rebase matching the prepared branch, \
                 original HEAD and pinned target base"
                    .to_string(),
            )
        };
        if input.get("recovery_kind").and_then(Value::as_str) != Some("vcs_conflict")
            || input.get("operation").and_then(Value::as_str) != Some("git_rebase")
        {
            return Err(invalid());
        }
        let prepared = input.get("failed_step_input").ok_or_else(invalid)?;
        let branch = prepared
            .get("head")
            .and_then(Value::as_str)
            .ok_or_else(invalid)?;
        let original = prepared
            .get("head_sha")
            .or_else(|| prepared.get("published_head_sha"))
            .and_then(Value::as_str)
            .ok_or_else(invalid)?;
        let original_base_sha = input
            .get("original_base_sha")
            .and_then(Value::as_str)
            .ok_or_else(invalid)?;
        let target = input
            .get("target_base_sha")
            .and_then(Value::as_str)
            .ok_or_else(invalid)?;
        let base_ref = prepared
            .get("base_ref")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .or_else(|| completion_recovery_base_ref(prepared))
            .ok_or_else(invalid)?;
        let prepared_target = prepared
            .get("base_sha")
            .and_then(Value::as_str)
            .unwrap_or(target);
        let step_id =
            validate_failed_step_identity(input, prepared, original).ok_or_else(invalid)?;
        // The stopped rebase is pinned by its own `onto` metadata below, not by
        // the moving base ref: a linked worktree shares remote-tracking refs
        // with every sibling checkout, so `origin/<base>` routinely advances
        // past the pin while the leaf runs. That advance is reconciled after
        // continuation, not refused here. The ref must still resolve so the
        // continuation can read its tip.
        git_stdout(&self.assigned_root, &["rev-parse", "--verify", &base_ref])?;
        if prepared_target != target
            || git_stdout(&self.assigned_root, &["merge-base", original, target])?
                != original_base_sha
        {
            return Err(invalid());
        }
        let conflicting_paths = recovery_conflicting_paths(input).ok_or_else(invalid)?;
        if unmerged_paths(&self.assigned_root)? != conflicting_paths {
            return Err(invalid());
        }
        let expected_ref = format!("refs/heads/{branch}");
        if git_stdout(
            &self.assigned_root,
            &["rev-parse", "--verify", &expected_ref],
        )? != original
        {
            return Err(invalid());
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
            if read("head-name").ok().as_deref() != Some(expected_ref.as_str())
                || read("orig-head").ok().as_deref() != Some(original)
                || read("onto").ok().as_deref() != Some(target)
            {
                return Err(invalid());
            }
            let metadata = RecoveryMetadata::capture(&self.assigned_root, path)?;
            let attempt = host.begin_rebase_recovery_attempt(
                &self.run_id,
                step_id,
                &RebaseRecoveryAttemptScope {
                    workspace_path: self.assigned_root.to_string_lossy().into_owned(),
                    head_sha_before: original.to_string(),
                    target_base_sha: target.to_string(),
                },
            )?;
            self.rebase_recovery = Some(RebaseRecoveryCheckpoint {
                metadata,
                step_id: step_id.to_string(),
                attempt,
                branch: branch.to_string(),
                original_head: original.to_string(),
                original_base_sha: original_base_sha.to_string(),
                base_ref,
                target_base_sha: target.to_string(),
                conflicting_paths,
                remote_sha_before: prepared
                    .get("remote_sha")
                    .or_else(|| prepared.get("published_head_sha"))
                    .and_then(Value::as_str)
                    .map(str::to_string),
            });
            return Ok(());
        }
        Err(invalid())
    }

    pub(super) fn completed_authorized_rebase(
        &self,
        after: &GitWorktreeFingerprint,
    ) -> Result<bool, DispatchError> {
        let Some(checkpoint) = &self.rebase_recovery else {
            return Ok(false);
        };
        if after.branch.as_deref() != Some(checkpoint.branch.as_str()) {
            return Ok(false);
        }
        for backend in ["rebase-merge", "rebase-apply"] {
            let path = git_stdout(
                &self.assigned_root,
                &["rev-parse", "--path-format=absolute", "--git-path", backend],
            )?;
            if Path::new(&path).exists() {
                return Ok(false);
            }
        }
        Ok(after.head != checkpoint.target_base_sha
            && git_output_raw(
                &self.assigned_root,
                &[
                    "merge-base",
                    "--is-ancestor",
                    &checkpoint.target_base_sha,
                    "HEAD",
                ],
            )?
            .success)
    }

    pub(super) fn complete_rebase_recovery(
        &self,
        host: &dyn RuntimeHost,
        step_id: &str,
        task_ids: &[String],
    ) -> Result<Value, DispatchError> {
        let checkpoint = self.rebase_recovery.as_ref().ok_or_else(|| {
            DispatchError::CliInvocationPermanent(
                "conflict recovery has no authenticated rebase checkpoint".to_string(),
            )
        })?;
        let invalid = |reason: &str| {
            DispatchError::CliInvocationPermanent(format!(
                "conflict recovery refused host Git continuation: {reason}"
            ))
        };
        if step_id != checkpoint.step_id {
            return Err(invalid(
                "the failed step differs from the admitted recovery",
            ));
        }

        // Authenticate metadata before snapshotting or staging repaired files.
        // Matching commits/index alone cannot authenticate a copy.
        checkpoint.metadata.verify(&self.assigned_root)?;
        let after_agent = git_fingerprint(&self.assigned_root)?;
        if after_agent.head != self.assigned_before.head
            || after_agent.branch != self.assigned_before.branch
            || !index_unchanged(&self.assigned_before, &after_agent)
        {
            return Err(invalid(
                "the provider changed HEAD, branch, or index instead of only repairing files",
            ));
        }
        let changed = changed_paths(&self.assigned_root, &self.assigned_before, &after_agent);
        if checkpoint
            .conflicting_paths
            .iter()
            .any(|path| !changed.contains(path))
        {
            return Err(invalid(
                "one or more authorized conflict files were not repaired",
            ));
        }
        // A resolution may need companion edits beyond the conflict set (a
        // re-export for a moved module, a caller of a renamed item). Every
        // other path the provider changed joins the continued commit; only
        // host-owned `.orbit/` state, and a pre-existing untracked file the
        // provider removed, stay out of it.
        let companion_paths = changed
            .iter()
            .filter(|path| !checkpoint.conflicting_paths.contains(path))
            .filter(|path| !is_host_owned_path(path))
            .filter(|path| {
                after_agent.path_states.get(*path).is_none_or(|state| {
                    state.worktree_present || state.index_entry_sha256.is_some()
                })
            })
            .cloned()
            .collect::<Vec<_>>();
        self.validate_rebase_checkpoint(checkpoint, &invalid)?;
        if unmerged_paths(&self.assigned_root)? != checkpoint.conflicting_paths {
            return Err(invalid(
                "the live unmerged paths changed after provider execution",
            ));
        }

        let mut diff_check_args = vec!["diff", "--check", "--"];
        diff_check_args.extend(checkpoint.conflicting_paths.iter().map(String::as_str));
        let diff_check = git_output_raw(&self.assigned_root, &diff_check_args)?;
        if !diff_check.success {
            return Err(invalid(
                "a repaired file still contains conflict markers or whitespace errors",
            ));
        }

        host.validate_step_recovery_mutation(&self.run_id, step_id, task_ids, &self.assigned_root)
            .map_err(|error| {
                invalid(&format!(
                    "live run/task/worktree ownership changed: {error}"
                ))
            })?;
        if let StepRecoveryAdmission::Denied { reason } = host
            .authorize_step_recovery(&self.run_id, step_id)
            .map_err(|error| invalid(&format!("recovery authorization recheck failed: {error}")))?
        {
            return Err(invalid(&format!(
                "recovery authorization was revoked: {reason}"
            )));
        }

        let mut add_args = vec!["add", "--all", "--"];
        add_args.extend(checkpoint.conflicting_paths.iter().map(String::as_str));
        add_args.extend(companion_paths.iter().map(String::as_str));
        git_mutation(&self.assigned_root, &add_args)?;
        if !unmerged_paths(&self.assigned_root)?.is_empty() {
            return Err(invalid(
                "staging the resolved conflict set left unresolved entries",
            ));
        }
        // Untracked files the continuation commits are no longer untracked;
        // every other pre-existing untracked payload must survive unchanged.
        let mut expected_untracked = self.assigned_before.untracked_content.clone();
        expected_untracked.retain(|path, _| !companion_paths.contains(path));
        let continued = git_mutation_output(
            &self.assigned_root,
            &["-c", "core.editor=true", "rebase", "--continue"],
        )?;
        if !continued.success {
            let additional = unmerged_paths(&self.assigned_root)?;
            let diagnostic = continued.stderr.trim().to_string();
            return Err(invalid(&format!(
                "rebase --continue failed; additional_conflicting_paths={additional:?}; diagnostic={diagnostic}"
            )));
        }

        let completed = git_fingerprint(&self.assigned_root)?;
        if !self.completed_authorized_rebase(&completed)? {
            return Err(invalid(
                "continued rebase did not leave the checkpointed branch on the pinned base with a candidate commit",
            ));
        }
        self.require_clean_continuation(&completed, &expected_untracked, &invalid)?;
        // The pin only records where the rebase stopped. Land the continued
        // candidate on the base tip that is live now, so the deterministic
        // retry and the PR see current integration rather than a stale one.
        let base_sha = self.follow_advanced_base(checkpoint, &expected_untracked, &invalid)?;
        let completed = git_fingerprint(&self.assigned_root)?;
        self.widen_task(
            host,
            task_ids,
            ContextWideningStep::Recovery,
            &companion_paths,
        );
        Ok(serde_json::json!({
            "run_id": self.run_id,
            "step_id": step_id,
            "task_ids": task_ids,
            "workspace_path": self.assigned_root,
            "head": checkpoint.branch,
            "head_sha_before": checkpoint.original_head,
            "original_base_sha": checkpoint.original_base_sha,
            "base_ref": checkpoint.base_ref,
            "target_base_sha": checkpoint.target_base_sha,
            "base_sha": base_sha,
            "remote_sha_before": checkpoint.remote_sha_before,
            "head_sha": completed.head,
            "companion_paths": companion_paths,
            "rewritten": true,
            "recovery_attempt": checkpoint.attempt,
        }))
    }

    /// The stopped index includes every nonconflicting candidate change.
    /// Those staged paths become clean when Git commits them; comparing dirty
    /// path maps across that transition incorrectly rejects the candidate.
    /// Provider edits were checked before continuation. Now require Git to
    /// leave no tracked dirt and preserve the pre-existing untracked payload
    /// the continuation did not commit (`expected_untracked`).
    fn require_clean_continuation(
        &self,
        completed: &GitWorktreeFingerprint,
        expected_untracked: &BTreeMap<String, String>,
        invalid: &impl Fn(&str) -> DispatchError,
    ) -> Result<(), DispatchError> {
        if !git_output_raw(&self.assigned_root, &["diff", "--quiet", "HEAD", "--"])?.success
            || &completed.untracked_content != expected_untracked
        {
            return Err(invalid(
                "host continuation left tracked dirt or changed untracked files",
            ));
        }
        Ok(())
    }

    /// Rebase the continued candidate onto `base_ref`'s current tip when it
    /// advanced past the pin, returning the base the candidate now sits on.
    ///
    /// A tip that does not descend from the pin (a rewound or force-moved
    /// base) is not followed; the pinned result stands and the deterministic
    /// retry judges it. A follow-up that conflicts is aborted so the branch
    /// keeps the already-resolved pinned result: the retry then reports the
    /// remaining advance instead of a lost resolution.
    fn follow_advanced_base(
        &self,
        checkpoint: &RebaseRecoveryCheckpoint,
        expected_untracked: &BTreeMap<String, String>,
        invalid: &impl Fn(&str) -> DispatchError,
    ) -> Result<String, DispatchError> {
        let pinned = checkpoint.target_base_sha.as_str();
        let live = git_stdout(
            &self.assigned_root,
            &["rev-parse", "--verify", &checkpoint.base_ref],
        )?;
        if live == pinned
            || !git_output_raw(
                &self.assigned_root,
                &["merge-base", "--is-ancestor", pinned, &live],
            )?
            .success
        {
            return Ok(pinned.to_string());
        }
        let followed = git_mutation_output(&self.assigned_root, &["rebase", &live])?;
        if !followed.success {
            let additional = unmerged_paths(&self.assigned_root)?;
            git_mutation(&self.assigned_root, &["rebase", "--abort"])?;
            let restored = git_fingerprint(&self.assigned_root)?;
            if !self.completed_authorized_rebase(&restored)? {
                return Err(invalid(
                    "aborting the follow-up rebase onto the advanced base did not restore the pinned result",
                ));
            }
            self.require_clean_continuation(&restored, expected_untracked, invalid)?;
            tracing::warn!(
                target: "orbit.engine.cli_runner",
                run_id = %self.run_id,
                pinned_base = pinned,
                advanced_base = %live,
                additional_conflicting_paths = ?additional,
                "conflict recovery kept the pinned result; the advanced base conflicts again"
            );
            return Ok(pinned.to_string());
        }
        let followed = git_fingerprint(&self.assigned_root)?;
        if !self.completed_authorized_rebase(&followed)?
            || followed.head == live
            || !git_output_raw(
                &self.assigned_root,
                &["merge-base", "--is-ancestor", &live, "HEAD"],
            )?
            .success
        {
            return Err(invalid(
                "rebasing onto the advanced base did not leave the checkpointed branch on that base with a candidate commit",
            ));
        }
        self.require_clean_continuation(&followed, expected_untracked, invalid)?;
        Ok(live)
    }

    fn validate_rebase_checkpoint(
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

/// In-memory host evidence, never loaded from provider output or scratch.
/// Hold directory handles so inode reuse cannot authenticate a replacement.
struct RecoveryMetadata {
    pointer: Vec<u8>,
    pointer_handle: fs::File,
    directories: Vec<(PathBuf, fs::File)>,
    rebase_root: PathBuf,
    rebase_files: BTreeMap<PathBuf, Vec<u8>>,
}

impl RecoveryMetadata {
    fn capture(root: &Path, rebase_root: &Path) -> Result<Self, DispatchError> {
        let mut directories = Vec::new();
        for flag in ["--absolute-git-dir", "--git-common-dir"] {
            let raw = git_stdout(root, &["rev-parse", "--path-format=absolute", flag])?;
            let path = PathBuf::from(raw).canonicalize().map_err(metadata_error)?;
            let handle = fs::File::open(&path).map_err(metadata_error)?;
            directories.push((path, handle));
        }
        Ok(Self {
            pointer: fs::read(root.join(".git")).map_err(metadata_error)?,
            pointer_handle: fs::File::open(root.join(".git")).map_err(metadata_error)?,
            directories,
            rebase_root: rebase_root.to_path_buf(),
            rebase_files: recovery_metadata_files(rebase_root)?,
        })
    }

    fn verify(&self, root: &Path) -> Result<(), DispatchError> {
        let invalid = || {
            DispatchError::CliInvocationPermanent(
                "conflict recovery refused changed Git metadata identity or recovery instructions"
                    .to_string(),
            )
        };
        if !same_metadata_entry(&root.join(".git"), &self.pointer_handle)?
            || fs::read(root.join(".git")).map_err(metadata_error)? != self.pointer
        {
            return Err(invalid());
        }
        for ((expected, handle), flag) in self
            .directories
            .iter()
            .zip(["--absolute-git-dir", "--git-common-dir"])
        {
            let raw = git_stdout(root, &["rev-parse", "--path-format=absolute", flag])?;
            let path = PathBuf::from(raw).canonicalize().map_err(metadata_error)?;
            if &path != expected {
                return Err(invalid());
            }
            if !same_metadata_entry(&path, handle)? {
                return Err(invalid());
            }
        }
        if recovery_metadata_files(&self.rebase_root)? != self.rebase_files {
            return Err(invalid());
        }
        Ok(())
    }
}

/// Whether the provider left the index as it found it. The fingerprint's
/// index identity covers only dirty paths, so a companion edit that dirties a
/// clean tracked file changes it without any staging; compare per path
/// instead. A path dirty on both sides keeps its index entry and staged
/// delta. A path dirty on one side only was clean (index equal to HEAD) on
/// the other, so it must carry no staged delta.
fn index_unchanged(before: &GitWorktreeFingerprint, after: &GitWorktreeFingerprint) -> bool {
    let paths = before
        .path_states
        .keys()
        .chain(after.path_states.keys())
        .collect::<BTreeSet<_>>();
    paths.into_iter().all(|path| {
        match (before.path_states.get(path), after.path_states.get(path)) {
            (Some(before), Some(after)) => {
                before.index_entry_sha256 == after.index_entry_sha256
                    && before.staged_patch_sha256 == after.staged_patch_sha256
            }
            (Some(state), None) | (None, Some(state)) => state.staged_patch_sha256.is_none(),
            (None, None) => true,
        }
    })
}

fn same_metadata_entry(path: &Path, handle: &fs::File) -> Result<bool, DispatchError> {
    let after = fs::symlink_metadata(path).map_err(metadata_error)?;
    if after.file_type().is_symlink() {
        return Ok(false);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        let before = handle.metadata().map_err(metadata_error)?;
        Ok((before.dev(), before.ino()) == (after.dev(), after.ino()))
    }
    #[cfg(not(unix))]
    {
        // Linux/macOS enforce inode identity; other platforms retain the
        // pointer, canonical-path and instruction-content checks above.
        let _ = handle;
        Ok(true)
    }
}

fn metadata_error(error: std::io::Error) -> DispatchError {
    DispatchError::CliInvocationPermanent(format!("inspect host recovery metadata: {error}"))
}

fn recovery_metadata_files(root: &Path) -> Result<BTreeMap<PathBuf, Vec<u8>>, DispatchError> {
    if !fs::symlink_metadata(root).map_err(metadata_error)?.is_dir() {
        return Err(DispatchError::CliInvocationPermanent(
            "host recovery metadata root is not a directory".to_string(),
        ));
    }
    let mut files = BTreeMap::new();
    for entry in fs::read_dir(root).map_err(metadata_error)? {
        let entry = entry.map_err(metadata_error)?;
        let kind = entry.file_type().map_err(metadata_error)?;
        if kind.is_dir() {
            files.extend(recovery_metadata_files(&entry.path())?);
        } else if kind.is_file() {
            let content = fs::read(entry.path()).map_err(metadata_error)?;
            files.insert(entry.path(), Sha256::digest(content).to_vec());
        } else {
            return Err(DispatchError::CliInvocationPermanent(
                "host recovery metadata contains a symlink or special file".to_string(),
            ));
        }
    }
    Ok(files)
}

/// The failed step a conflict recovery may complete, when its input
/// describes one of the two rebasing steps consistently.
fn validate_failed_step_identity<'a>(
    input: &'a Value,
    prepared: &Value,
    original: &str,
) -> Option<&'a str> {
    let activity_name = input.get("activity_name")?.as_str()?;
    let failed_step_id = input.get("failed_step_id")?.as_str()?;
    if !matches!(
        (failed_step_id, activity_name),
        ("sync_base", "git_rebase") | ("complete_pr", "pr_complete")
    ) {
        return None;
    }
    if activity_name == "pr_complete"
        && (prepared.get("completion")?.as_str()? != "done"
            || prepared.get("published_head_sha")?.as_str()? != original
            || prepared.get("pr_number")?.as_str()?.is_empty())
    {
        return None;
    }
    Some(failed_step_id)
}

fn recovery_conflicting_paths(input: &Value) -> Option<Vec<String>> {
    let values = input.get("conflicting_paths")?.as_array()?;
    let mut paths = Vec::with_capacity(values.len());
    for value in values {
        let path = value.as_str()?;
        safe_relative_path(path).ok()?;
        paths.push(path.to_string());
    }
    if paths.is_empty() {
        return None;
    }
    let original_len = paths.len();
    paths.sort();
    paths.dedup();
    (paths.len() == original_len).then_some(paths)
}

fn completion_recovery_base_ref(prepared: &Value) -> Option<String> {
    let base = prepared.get("base")?.as_str()?;
    match prepared.get("base_sync").and_then(Value::as_str) {
        Some("local") => Some(base.to_string()),
        Some("remote") | None => Some(format!(
            "refs/remotes/origin/{}",
            base.strip_prefix("origin/").unwrap_or(base)
        )),
        Some(_) => None,
    }
}

fn unmerged_paths(root: &Path) -> Result<Vec<String>, DispatchError> {
    let mut paths = nul_paths(&git_stdout_bytes(
        root,
        &["diff", "--name-only", "-z", "--diff-filter=U", "--"],
    )?);
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn git_mutation(root: &Path, args: &[&str]) -> Result<(), DispatchError> {
    let output = git_mutation_output(root, args)?;
    if output.success {
        Ok(())
    } else {
        Err(git_command_error(root, args, &output))
    }
}

fn git_mutation_output(root: &Path, args: &[&str]) -> Result<GitBytesOutcome, DispatchError> {
    let output = git_run_bytes(root, args, None).map_err(|error| {
        DispatchError::CliInvocationPermanent(format!(
            "mutate Git state in '{}' with `git {}`: {error}",
            root.display(),
            args.join(" ")
        ))
    })?;
    if output.timed_out {
        return Err(DispatchError::GitTimeout {
            operation: args.join(" "),
            root: root.to_path_buf(),
            timeout_ms: output.timeout_ms,
            diagnostic: output.stderr.trim().to_string(),
        });
    }
    Ok(output)
}
