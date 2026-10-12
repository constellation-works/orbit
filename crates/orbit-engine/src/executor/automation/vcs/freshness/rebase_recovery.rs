//! Interrupted rebase recovery and certified rewrite classification.
use super::super::super::input::{input_string_field, required_input_string};
use super::super::absorbed::{
    ABSORBING_STEP, AbsorbedReason, AbsorbedRefusal, covering_commit, task_scope_digest,
};
use super::super::git::{
    git_command_success, git_output, git_output_raw, git_success, git_timeout_error,
    timeout_recovery_error,
};
use super::super::handoff::{HandoffContext, rebase_in_progress};
use super::checkpoint::{RecoveryCheckpointLookup, recovery_checkpoint_lookup, recovery_run_id};
use super::divergence::original_base_sha;
use crate::context::{RebaseRecoveryAttemptScope, RuntimeHost};
use orbit_common::{OrbitError, RecoverableVcsConflict};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// Certify a deterministic `sync_base` rebase that dropped every pick, using
/// the same content evidence and host authority as conflict recovery. Other
/// steps and candidates without a covering commit retain the empty-branch
/// refusal. Consumers re-observe this certificate before settlement.
pub(super) fn certify_empty_rebase<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
    context: &HandoffContext,
    base_sha: &str,
) -> Result<Value, OrbitError> {
    let refused =
        |reason, detail| OrbitError::Execution(AbsorbedRefusal::new(reason, detail).to_string());
    if input.get("step_id").and_then(Value::as_str) != Some(ABSORBING_STEP)
        || context.tasks.len() != 1
    {
        return Err(refused(
            AbsorbedReason::StaleCheckpoint,
            "only a single-task sync_base may settle an empty rebase",
        ));
    }
    let workspace = context.workspace_path.as_path();
    if !git_output_raw(
        workspace,
        &["status", "--porcelain", "--untracked-files=all"],
    )?
    .is_empty()
    {
        return Err(refused(
            AbsorbedReason::DirtyWorktree,
            "the rebased checkout has uncommitted or untracked changes",
        ));
    }
    let head_sha_before = required_input_string(input, "head_sha")?;
    let original_base = original_base_sha(workspace, head_sha_before, base_sha)?;
    let (covering, candidate_paths) =
        covering_commit(workspace, &original_base, head_sha_before, base_sha)?.ok_or_else(
            || {
                refused(
                    AbsorbedReason::NoCoveringCommit,
                    "the synced base does not hold the candidate's exact content",
                )
            },
        )?;
    let run_id = recovery_run_id(input, context);
    let workspace_path = workspace.to_string_lossy().into_owned();
    let dispatch =
        |error: crate::activity_job::DispatchError| OrbitError::Execution(error.to_string());
    let attempt = host
        .begin_rebase_recovery_attempt(
            run_id,
            ABSORBING_STEP,
            &RebaseRecoveryAttemptScope {
                workspace_path: workspace_path.clone(),
                head_sha_before: head_sha_before.to_string(),
                target_base_sha: base_sha.to_string(),
            },
        )
        .map_err(dispatch)?;
    let checkpoint = json!({
        "run_id": run_id,
        "step_id": ABSORBING_STEP,
        "task_ids": [context.tasks[0].id],
        "workspace_path": workspace_path,
        "head": input["head"],
        "head_sha_before": head_sha_before,
        "original_base_sha": original_base,
        "base_ref": input["base_ref"],
        "target_base_sha": base_sha,
        "base_sha": base_sha,
        "remote_sha_before": input.get("remote_sha").cloned().unwrap_or(Value::Null),
        "head_sha": base_sha,
        "companion_paths": [],
        "rewritten": true,
        "recovery_attempt": attempt,
        "absorbed": {
            "covering_commit": covering,
            "candidate_paths": candidate_paths,
            "task_scope_digest": task_scope_digest(&context.tasks[0]),
        },
    });
    host.checkpoint_rebase_recovery(run_id, ABSORBING_STEP, &checkpoint)
        .map_err(dispatch)?;
    Ok(checkpoint)
}

pub(in crate::executor::automation::vcs) fn ensure_clean_for_rewrite(
    workspace_path: &Path,
    head_sha_before: &str,
    what: &str,
) -> Result<(), OrbitError> {
    let dirty_paths = dirty_worktree_paths(workspace_path)?;
    if !dirty_paths.is_empty() {
        return Err(OrbitError::Execution(format!(
            "git_rebase: refusing to rewrite {what} on top of checkpoint '{head_sha_before}': \
             the worktree has uncommitted changes that a rebase or reset would destroy: {}",
            dirty_paths.join(", ")
        )));
    }
    Ok(())
}

/// Every path `git status --porcelain` reports as staged, unstaged, or
/// untracked. A nonempty result means a hard reset could destroy something
/// that is not recorded anywhere else.
///
/// Uses [`git_output_raw`] rather than [`git_output`]: the latter trims the
/// whole output, which would eat the leading status column of a single-line
/// result (` M path` -> `M path`) and misalign every path by one byte.
fn dirty_worktree_paths(workspace_path: &Path) -> Result<Vec<String>, OrbitError> {
    Ok(git_output_raw(workspace_path, &["status", "--porcelain"])?
        .lines()
        .filter_map(|line| line.get(3..))
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(ToOwned::to_owned)
        .collect())
}

pub(super) fn refuse_or_recover_existing_rebase(
    workspace_path: &Path,
    head: &str,
    head_sha_before: &str,
    base_sha: &str,
) -> Result<Value, OrbitError> {
    let conflicting_paths = unmerged_paths(workspace_path)?;
    if !conflicting_paths.is_empty() {
        // A conflict recovery round can leave this candidate's rebase stopped
        // on a later pick. It stays pinned to its own `onto`, which a
        // completion retry's fresh base fetch may already have passed; the
        // next round must describe that pin to be admitted.
        let onto = read_rebase_state(workspace_path, "onto")?;
        let target_base_sha = match onto.as_deref() {
            Some(onto)
                if rebase_belongs_to_attempt(workspace_path, head, head_sha_before, onto)? =>
            {
                onto
            }
            _ => base_sha,
        };
        // Naming the stopped pick tells a later stop of the same rebase
        // apart from the one a recovery round just resolved.
        let diagnostic = match (
            read_rebase_state(workspace_path, "msgnum")?,
            read_rebase_state(workspace_path, "end")?,
        ) {
            (Some(pick), Some(end)) => {
                format!(
                    "rebase remains stopped with unresolved conflicts at commit {pick} of {end}"
                )
            }
            _ => "rebase remains stopped with unresolved conflicts".to_string(),
        };
        return Err(rebase_conflict_error(
            workspace_path,
            head_sha_before,
            target_base_sha,
            conflicting_paths,
            &diagnostic,
        )?);
    }
    if rebase_belongs_to_attempt(workspace_path, head, head_sha_before, base_sha)? {
        abort_owned_rebase(workspace_path)?;
        return Err(OrbitError::Execution(
            "git_rebase: interrupted rebase started by this attempt was aborted after a Git timeout. Retry can start clean. This is timeout recovery, not a merge conflict and not failure-handoff recovery.".to_string(),
        ));
    }
    let provenance = rebase_provenance_summary(workspace_path);
    Err(OrbitError::Execution(format!(
        "git_rebase: a pre-existing rebase is in progress without unresolved conflict entries ({provenance}). Not aborting; foreign or retained candidate state was left intact. Inspect the worktree before retrying. This is not conflict recovery."
    )))
}

pub(super) fn recover_started_rebase_timeout<T>(
    workspace_path: &Path,
    head: &str,
    head_sha_before: &str,
    base_sha: &str,
    outcome: &super::super::git::GitOutcome,
) -> Result<T, OrbitError> {
    let timeout = git_timeout_error(
        workspace_path,
        &["rebase", base_sha],
        outcome.timeout_ms,
        &outcome.stderr,
    );
    if !rebase_in_progress(workspace_path).unwrap_or(false) {
        return Err(timeout_recovery_error(
            outcome.timeout_ms,
            format!(
                "{timeout}; git_rebase of '{head}' onto '{base_sha}' timed out. This is timeout recovery, not a merge conflict and not failure-handoff recovery."
            ),
        ));
    }
    let conflicting_paths = unmerged_paths(workspace_path).unwrap_or_default();
    if !conflicting_paths.is_empty() {
        return Err(rebase_conflict_error(
            workspace_path,
            head_sha_before,
            base_sha,
            conflicting_paths,
            &format!(
                "rebase of '{head}' onto checkpoint '{base_sha}' timed out while stopped with conflicts"
            ),
        )?);
    }
    abort_owned_rebase(workspace_path)?;
    Err(timeout_recovery_error(
        outcome.timeout_ms,
        format!(
            "{timeout}; interrupted rebase started by this attempt was aborted. Retry can start clean. This is timeout recovery, not a merge conflict and not failure-handoff recovery."
        ),
    ))
}

pub(in crate::executor::automation::vcs) fn abort_owned_rebase(
    workspace_path: &Path,
) -> Result<(), OrbitError> {
    git_success(workspace_path, &["rebase", "--abort"]).map_err(|error| {
        OrbitError::Execution(format!(
            "git_rebase: failed to abort an interrupted rebase started by this attempt: {error}"
        ))
    })
}

pub(in crate::executor::automation::vcs) fn rebase_belongs_to_attempt(
    workspace_path: &Path,
    head: &str,
    head_sha_before: &str,
    base_sha: &str,
) -> Result<bool, OrbitError> {
    let orig_head = read_rebase_state(workspace_path, "orig-head")?;
    let onto = read_rebase_state(workspace_path, "onto")?;
    let head_name = read_rebase_state(workspace_path, "head-name")?;
    let orig_ok = orig_head.as_deref() == Some(head_sha_before);
    let onto_ok = onto.as_deref() == Some(base_sha);
    let head_ok = head_name.as_deref().is_some_and(|name| {
        name == head || name == format!("refs/heads/{head}") || name.ends_with(&format!("/{head}"))
    });
    Ok(orig_ok && onto_ok && head_ok)
}

pub(in crate::executor::automation::vcs) fn rebase_provenance_summary(
    workspace_path: &Path,
) -> String {
    let orig_head = read_rebase_state(workspace_path, "orig-head")
        .ok()
        .flatten()
        .unwrap_or_else(|| "unknown".to_string());
    let onto = read_rebase_state(workspace_path, "onto")
        .ok()
        .flatten()
        .unwrap_or_else(|| "unknown".to_string());
    let head_name = read_rebase_state(workspace_path, "head-name")
        .ok()
        .flatten()
        .unwrap_or_else(|| "unknown".to_string());
    format!("orig-head={orig_head}, onto={onto}, head-name={head_name}")
}

fn read_rebase_state(workspace_path: &Path, name: &str) -> Result<Option<String>, OrbitError> {
    for dir in ["rebase-merge", "rebase-apply"] {
        let rel = match git_output(
            workspace_path,
            &["rev-parse", "--git-path", &format!("{dir}/{name}")],
        ) {
            Ok(value) => value,
            Err(_) => continue,
        };
        let path = if Path::new(&rel).is_absolute() {
            PathBuf::from(rel)
        } else {
            workspace_path.join(rel)
        };
        if let Ok(contents) = std::fs::read_to_string(&path) {
            let trimmed = contents.trim();
            if !trimmed.is_empty() {
                return Ok(Some(trimmed.to_string()));
            }
        }
    }
    Ok(None)
}

/// How `current_sha` relates to the host-certified recovery checkpoints of
/// the prepared rewrite.
///
/// - `Current`: a certified checkpoint matches and its provenance agrees
///   with this attempt; the caller may reuse `current_sha` and must judge
///   freshness against the checkpoint's `base_sha`.
/// - `Uncertified`: a checkpoint matching this exact HEAD exists but carries
///   no certificate (a pre-authority-boundary checkpoint and a forged one look
///   identical here). The caller must not inherit `current_sha`; discarding
///   it and redoing the rebase is safe precisely because a real redo either
///   reproduces an equivalent, freshly self-verified result or hits the same
///   conflicts a forged shortcut was trying to skip.
/// - `Absent`: nothing at all backs this changed HEAD (the ordinary
///   "unexplained rewrite" refusal).
/// - `Overtaken`: a certified checkpoint matches this attempt in everything
///   but its base, and the prepared base strictly descends from the base the
///   checkpoint landed on: the base advanced after recovery certified it. The
///   caller chases the advance ([`super::super::base_chase::chase_advanced_base`]) rather than reusing
///   or refusing it [ORB-14393].
/// - `Err`: a certified checkpoint was found whose recorded provenance does
///   not match this attempt. A hard refusal with no redo.
pub(super) fn recovered_rewrite<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
    context: &HandoffContext,
    current_sha: &str,
) -> Result<RecoveredRewrite, OrbitError> {
    let run_id = recovery_run_id(input, context);
    let checkpoint =
        match recovery_checkpoint_lookup(host, run_id, &context.workspace_path, current_sha)? {
            RecoveryCheckpointLookup::Certified(checkpoint) => checkpoint,
            RecoveryCheckpointLookup::Uncertified => return Ok(RecoveredRewrite::Uncertified),
            RecoveryCheckpointLookup::Absent => return Ok(RecoveredRewrite::Absent),
        };
    let task_ids = context
        .tasks
        .iter()
        .map(|task| task.id.as_str())
        .collect::<Vec<_>>();
    let mismatch = || {
        OrbitError::Execution(
            "git_rebase: recovered HEAD provenance does not match the prepared rewrite checkpoint"
                .to_string(),
        )
    };
    if checkpoint["head"] != input["head"]
        || checkpoint["head_sha_before"] != input["head_sha"]
        || checkpoint["remote_sha_before"]
            != input.get("remote_sha").cloned().unwrap_or(Value::Null)
        || checkpoint["task_ids"] != json!(task_ids)
    {
        return Err(mismatch());
    }
    if recovery_pinned_base(&checkpoint, &input["base_sha"]) {
        return Ok(RecoveredRewrite::Current(checkpoint));
    }
    if base_overtook_recovery(input, &context.workspace_path, &checkpoint)? {
        return Ok(RecoveredRewrite::Overtaken(checkpoint));
    }
    Err(mismatch())
}

/// Whether the prepared base strictly descends from the base `checkpoint`
/// landed on, for the step that certified it.
fn base_overtook_recovery(
    input: &Value,
    workspace: &Path,
    checkpoint: &Value,
) -> Result<bool, OrbitError> {
    let (Some(prepared), Some(landed)) = (
        input_string_field(input, "base_sha"),
        input_string_field(checkpoint, "base_sha"),
    ) else {
        return Ok(false);
    };
    // A chase is certified under the step that certified the recovery, so a
    // different step's checkpoint is never chased from.
    if input
        .get("step_id")
        .is_some_and(|step| *step != checkpoint["step_id"])
    {
        return Ok(false);
    }
    Ok(prepared != landed
        && git_command_success(
            workspace,
            &["merge-base", "--is-ancestor", &landed, &prepared],
        )?)
}

/// How the current HEAD relates to the certified recovery of the prepared
/// rewrite; see [`recovered_rewrite`].
pub(super) enum RecoveredRewrite {
    Current(Value),
    Overtaken(Value),
    Uncertified,
    Absent,
}

/// Whether a recovery checkpoint was produced for the prepared base pin.
///
/// The host records `target_base_sha` (the pin the stopped rebase continued
/// onto) separately from `base_sha` (the base it then landed the candidate
/// on, which may be a later tip). A `sync_base` retry still carries the pin;
/// a `complete_pr` retry re-pins to the tip it fetched. Older checkpoints
/// carry only `base_sha`, which was always the pin.
pub(in crate::executor::automation::vcs) fn recovery_pinned_base(
    checkpoint: &Value,
    prepared_base_sha: &Value,
) -> bool {
    checkpoint["base_sha"] == *prepared_base_sha
        || checkpoint
            .get("target_base_sha")
            .is_some_and(|pinned| pinned == prepared_base_sha)
}

pub(in crate::executor::automation::vcs) fn unmerged_paths(
    repo_root: &Path,
) -> Result<Vec<String>, OrbitError> {
    Ok(
        git_output_raw(repo_root, &["diff", "--name-only", "--diff-filter=U", "-z"])?
            .split('\0')
            .filter(|path| !path.is_empty())
            .map(ToOwned::to_owned)
            .collect(),
    )
}

pub(super) fn rebase_conflict_error(
    repo_root: &Path,
    head_sha_before: &str,
    target_base_sha: &str,
    conflicting_paths: Vec<String>,
    diagnostic: &str,
) -> Result<OrbitError, OrbitError> {
    Ok(OrbitError::RecoverableVcsConflict(Box::new(
        RecoverableVcsConflict {
            operation: "git_rebase".to_string(),
            original_base_sha: original_base_sha(repo_root, head_sha_before, target_base_sha)?,
            target_base_sha: target_base_sha.to_string(),
            conflicting_files: conflicted_files(repo_root, &conflicting_paths)?,
            conflicting_paths,
            diagnostic: diagnostic.to_string(),
        },
    )))
}

/// Only trivial per-path three-way resolutions are proved here. Distinct edits,
/// renames, and absent or malformed index evidence remain for recovery to judge.
fn conflicted_files(
    repo_root: &Path,
    paths: &[String],
) -> Result<Vec<orbit_common::ConflictedFile>, OrbitError> {
    // One index read for the whole stop, rather than one Git process per file.
    let raw = git_output_raw(repo_root, &["ls-files", "--unmerged", "-z"])?;
    let mut entries = std::collections::BTreeMap::new();
    let mut valid = true;
    for record in raw.split('\0').filter(|record| !record.is_empty()) {
        let Some((entry, path)) = record.split_once('\t') else {
            valid = false;
            continue;
        };
        let fields = entry.split_whitespace().collect::<Vec<_>>();
        let stage = fields.get(2).and_then(|stage| stage.parse::<usize>().ok());
        if fields.len() != 3 || !matches!(stage, Some(1..=3)) {
            valid = false;
            continue;
        }
        let stages = entries.entry(path).or_insert([None, None, None]);
        if let Some(stage) = stage
            && stages[stage - 1].replace((fields[0], fields[1])).is_some()
        {
            valid = false;
        }
    }
    Ok(paths
        .iter()
        .map(|path| {
            let [base, ours, theirs] = entries.get(path.as_str()).copied().unwrap_or_default();
            // A missing stage is a deletion, but two absent stages do not prove
            // agreement. Require real matching entries on at least two sides.
            let identical = |left: Option<(&str, &str)>, right| left.is_some() && left == right;
            let deterministic = valid
                && (identical(ours, theirs) || identical(base, ours) || identical(base, theirs));
            orbit_common::ConflictedFile {
                path: path.clone(),
                deterministically_resolvable: deterministic,
            }
        })
        .collect())
}
