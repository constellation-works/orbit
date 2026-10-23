//! Exact host commit/synchronization provenance retained during final replay.
use std::path::Path;

use super::super::super::input::input_string_field;
use super::checkpoint::pinned_object_id;
use crate::context::RuntimeHost;
use orbit_common::OrbitError;
use orbit_types::workflow::{FinalRecoveryDecision, JobRunState, PipelineState};
use serde_json::{Value, json};

use super::super::git::git_output;
use super::checkpoint::head_descends_from_pin;

/// These checkpoints prove history, not validation or review. A resumed phase
/// reruns their actions; retaining them never permits skipping delivery gates.
pub(crate) const PUBLISHED_REPLAY_STEPS: [(&str, &str); 5] = [
    ("commit", "git_commit"),
    ("prepare_branch", "pr_prepare"),
    ("sync_base", "git_rebase"),
    ("push", "git_push"),
    ("pr_open", "pr_open"),
];

fn checkpoint<'a>(state: &'a PipelineState, id: &str) -> Option<&'a Value> {
    let output = state.pipeline.get(id)?;
    state.step_outputs.iter().find_map(|(index, recorded)| {
        (recorded == output && state.step_states.get(index) == Some(&JobRunState::Success))
            .then_some(output)
    })
}

/// Authenticate a published HEAD or its exact host replay continuation. Neither
/// ancestry alone nor observing the current HEAD creates this authority. The
/// run's final-recovery receipt and task ownership are checked by the caller.
pub(crate) fn published_candidate_matches(
    state: &PipelineState,
    owner: &str,
    task: &str,
    workspace: &Path,
    pin: &str,
    head: &str,
) -> Result<bool, OrbitError> {
    let text =
        |value: &Value, key: &str| value.get(key).and_then(Value::as_str).map(str::to_string);
    let Some(worktree) = checkpoint(state, "worktree") else {
        return Ok(false);
    };
    let Some(commit) = checkpoint(state, "commit") else {
        return Ok(false);
    };
    let canonical = workspace.canonicalize()?;
    let git_root = git_output(workspace, &["rev-parse", "--show-toplevel"])?;
    if Path::new(&git_root) != canonical.as_path() {
        return Ok(false);
    }
    let branch = git_output(workspace, &["symbolic-ref", "--quiet", "--short", "HEAD"])?;
    let Some(committed) = text(commit, "commit_sha") else {
        return Ok(false);
    };

    let root = text(worktree, "workspace_path").map(std::path::PathBuf::from);
    if state.run_id != owner
        || text(worktree, "job_run_id").as_deref() != Some(owner)
        || text(worktree, "base_sha").as_deref() != Some(pin)
        || root
            .as_deref()
            .and_then(|path| path.canonicalize().ok())
            .as_ref()
            != Some(&canonical)
        || text(commit, "task_id").as_deref() != Some(task)
        || text(commit, "job_run_id").as_deref() != Some(owner)
        || text(commit, "base_sha").as_deref() != Some(pin)
        || !matches!(
            text(commit, "decision").as_deref(),
            Some("performed" | "already_committed")
        )
    {
        return Ok(false);
    }
    // A replay that has crossed its host commit boundary already records the
    // exact current branch and candidate, including a file repair. This also
    // survives interruption before preparation/synchronization rerun.
    let replay = commit.get("final_recovery_replay").and_then(Value::as_bool) == Some(true);
    if replay {
        if text(commit, "candidate_branch").as_deref() != Some(&branch) {
            return Ok(false);
        }
        if committed == head {
            return head_descends_from_pin(workspace, pin, head);
        }
    }
    let (Some(prepared), Some(sync)) = (
        checkpoint(state, "prepare_branch"),
        checkpoint(state, "sync_base"),
    ) else {
        return Ok(false);
    };
    let Some(base) = text(sync, "base_sha") else {
        return Ok(false);
    };
    let Some(base_branch) = text(sync, "base") else {
        return Ok(false);
    };
    if text(prepared, "head").as_deref() != Some(&branch)
        || text(sync, "head").as_deref() != Some(&branch)
        || text(prepared, "base").as_deref() != Some(&base_branch)
        || text(prepared, "base_sha").as_deref() != Some(&base)
        || text(prepared, "head_sha").as_deref() != Some(&committed)
        || text(sync, "head_sha_before").as_deref() != Some(&committed)
        || text(sync, "head_sha").as_deref() != Some(head)
        || sync.get("rewritten").and_then(Value::as_bool) != Some(committed != head)
        || !head_descends_from_pin(workspace, pin, &committed)?
        || !head_descends_from_pin(workspace, &base, head)?
    {
        return Ok(false);
    }
    // Once an authenticated replay crosses the host commit boundary, its
    // preparation and rebase receipts authenticate the next exact HEAD. Push
    // and PR checkpoints may still belong to the preceding publication when
    // interrupted here. They convey no authority over this new candidate.
    if replay {
        return Ok(true);
    }
    let (Some(push), Some(pr)) = (checkpoint(state, "push"), checkpoint(state, "pr_open")) else {
        return Ok(false);
    };
    if text(push, "branch").as_deref() != Some(&branch)
        || text(pr, "head").as_deref() != Some(&branch)
        || text(pr, "base").as_deref() != Some(&base_branch)
        || text(pr, "base_sha").as_deref() != Some(&base)
        || text(push, "local_sha").as_deref() != Some(head)
        || text(pr, "pr_number").is_none_or(|number| number.is_empty())
    {
        return Ok(false);
    }
    Ok(true)
}

/// The governed alternative to comparing a published candidate against setup.
/// Unknown HEADs still go through the unchanged immutable-pin refusal.
pub(super) fn final_recovery_published_candidate<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
    owner: &str,
    task: &str,
    workspace: &Path,
) -> Result<Option<(String, String)>, OrbitError> {
    let Some(pin) = input_string_field(input, "base_sha") else {
        return Ok(None);
    };
    let pin = pinned_object_id(&pin)?;
    let head = git_output(workspace, &["rev-parse", "--verify", "HEAD^{commit}"])?;
    if pin == head {
        return Ok(None);
    }
    let active = input_string_field(input, "run_id").unwrap_or_else(|| owner.to_string());
    if active != owner {
        return Ok(None);
    }
    let Some(state) = host.read_run_state(&active)? else {
        return Ok(None);
    };
    let Some(recovery) = &state.final_recovery else {
        return Ok(None);
    };
    if recovery.key.run_id != active
        || recovery.task_id != task
        || recovery.outcome.as_deref() != Some("resume")
        || recovery.repair_commit.is_some()
        || !matches!(
            recovery.decision,
            Some(FinalRecoveryDecision::Resume { .. })
        )
        || host.get_task(task)?.job_run_id.as_deref() != Some(owner)
    {
        return Ok(None);
    }
    Ok(
        published_candidate_matches(&state, owner, task, workspace, &pin, &head)?
            .then_some((pin, head)),
    )
}

/// A replay may rerun synchronization after an interrupted rebase and find
/// nothing left to rewrite. Keep publication leased to the preceding host
/// push, rather than treating that no-op as authority to overwrite a remote.
pub(in crate::executor::automation::vcs) fn final_recovery_publication_lease<
    H: RuntimeHost + ?Sized,
>(
    host: &H,
    input: &Value,
    workspace: &Path,
    branch: &str,
    head: &str,
) -> Result<Option<String>, OrbitError> {
    let Some(owner) = input_string_field(input, "job_run_id") else {
        return Ok(None);
    };
    let Some(state) = host.read_run_state(&owner)? else {
        return Ok(None);
    };
    let (Some(worktree), Some(commit), Some(push), Some(pr)) = (
        checkpoint(&state, "worktree"),
        checkpoint(&state, "commit"),
        checkpoint(&state, "push"),
        checkpoint(&state, "pr_open"),
    ) else {
        return Ok(None);
    };
    if commit["final_recovery_replay"] != true
        || commit["candidate_branch"] != branch
        || push["branch"] != branch
        || pr["head"] != branch
        || pr["pr_number"].as_str().is_none_or(str::is_empty)
    {
        return Ok(None);
    }
    let (Some(task), Some(pin), Some(lease)) = (
        commit["task_id"].as_str(),
        worktree["base_sha"].as_str(),
        push["local_sha"].as_str(),
    ) else {
        return Ok(None);
    };
    let mut authentication = input.clone();
    authentication["base_sha"] = json!(pin);
    let authenticated =
        final_recovery_published_candidate(host, &authentication, &owner, task, workspace)?;
    Ok(authenticated
        .filter(|(_, candidate)| candidate == head)
        .map(|_| lease.to_string()))
}
