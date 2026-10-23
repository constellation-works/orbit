use std::collections::BTreeSet;
use std::path::Path;

use serde_json::Value;

use crate::context::{RuntimeHost, StepRecoveryAdmission};
use crate::executor::automation::vcs::git::{BaseSyncMode, GitBytesOutcome, git_run_bytes};

use super::super::DispatchError;
use super::super::fingerprint::{
    GitWorktreeFingerprint, git_command_error, git_output_raw, git_stdout_bytes, nul_paths,
};
use super::super::recovery::safe_relative_path;

/// Whether the provider left the index as it found it. The fingerprint's
/// index identity covers only dirty paths, so a companion edit that dirties a
/// clean tracked file changes it without any staging; compare per path
/// instead. A path dirty on both sides keeps its index entry and staged
/// delta. A path dirty on one side only was clean (index equal to HEAD) on
/// the other, so it must carry no staged delta.
pub(super) fn index_unchanged(
    before: &GitWorktreeFingerprint,
    after: &GitWorktreeFingerprint,
) -> bool {
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

/// The failed step a conflict recovery may complete, when its input
/// describes one of the rebasing steps consistently: `sync_base`, or a
/// completion that rebases a conflicting published PR (`complete_pr`, and
/// `complete_reviewed_pr` once the base moved again after a re-review).
pub(super) fn validate_failed_step_identity<'a>(
    input: &'a Value,
    prepared: &Value,
    original: &str,
) -> Option<&'a str> {
    let activity_name = input.get("activity_name")?.as_str()?;
    let failed_step_id = input.get("failed_step_id")?.as_str()?;
    if !matches!(
        (failed_step_id, activity_name),
        ("sync_base", "git_rebase")
            | ("complete_pr", "pr_complete")
            | ("complete_reviewed_pr", "pr_complete")
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

pub(super) fn recovery_conflicting_paths(input: &Value) -> Option<Vec<String>> {
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

pub(super) fn completion_recovery_base_ref(prepared: &Value) -> Option<String> {
    let base = prepared.get("base")?.as_str()?;
    match BaseSyncMode::from_input(prepared).ok()? {
        BaseSyncMode::Local => Some(base.to_string()),
        BaseSyncMode::Remote => Some(format!(
            "refs/remotes/origin/{}",
            base.strip_prefix("origin/").unwrap_or(base)
        )),
    }
}

/// Whether HEAD still has the shape a stopped rebase gives it: detached, on
/// `onto` or a pick replayed onto it. Earlier repairs are adopted only from
/// file edits; an actor that re-attached or moved HEAD is not continued.
pub(super) fn stopped_head_on_pin(root: &Path, onto: &str) -> Result<bool, DispatchError> {
    Ok(
        !git_output_raw(root, &["symbolic-ref", "--quiet", "HEAD"])?.success
            && git_output_raw(root, &["merge-base", "--is-ancestor", onto, "HEAD"])?.success,
    )
}

pub(super) fn unmerged_paths(root: &Path) -> Result<Vec<String>, DispatchError> {
    let mut paths = nul_paths(&git_stdout_bytes(
        root,
        &["diff", "--name-only", "-z", "--diff-filter=U", "--"],
    )?);
    paths.sort();
    paths.dedup();
    Ok(paths)
}

/// Whether the live run, task and worktree still own this step recovery and
/// its authorization stands; rechecked just before the host writes the
/// checkout.
pub(super) fn recheck_recovery_owner(
    host: &dyn RuntimeHost,
    run_id: &str,
    step_id: &str,
    task_ids: &[String],
    root: &Path,
    invalid: &dyn Fn(&str) -> DispatchError,
) -> Result<(), DispatchError> {
    host.validate_step_recovery_mutation(run_id, step_id, task_ids, root)
        .map_err(|error| {
            invalid(&format!(
                "live run/task/worktree ownership changed: {error}"
            ))
        })?;
    if let StepRecoveryAdmission::Denied { reason } = host
        .authorize_step_recovery(run_id, step_id)
        .map_err(|error| invalid(&format!("recovery authorization recheck failed: {error}")))?
    {
        return Err(invalid(&format!(
            "recovery authorization was revoked: {reason}"
        )));
    }
    Ok(())
}

pub(super) fn git_mutation(root: &Path, args: &[&str]) -> Result<(), DispatchError> {
    let output = git_mutation_output(root, args)?;
    if output.success {
        Ok(())
    } else {
        Err(git_command_error(root, args, &output))
    }
}

pub(in crate::activity_job::workspace) fn git_mutation_output(
    root: &Path,
    args: &[&str],
) -> Result<GitBytesOutcome, DispatchError> {
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
