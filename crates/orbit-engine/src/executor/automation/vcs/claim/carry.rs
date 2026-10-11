//! Carry a failed claimed leaf's candidate to a durable ref
//! [ORB-14338].
//!
//! `claim_candidate_carry` is the claimed PR leaf's terminal
//! `failure_activity`. A leaf that committed a candidate and failed before its
//! push leaves that candidate only in this host's object store, where a claim
//! on any other host cannot fetch it. The hook pushes the candidate's branch
//! tip — review fixes included — to `refs/orbit/candidates/<task>/<run>` on
//! `origin`, and its output, checkpointed on the run, tells the drain's
//! failure settlement where the candidate is: `published` (the leaf's own
//! push already put its branch on `origin`), `durable` (the ref it pushed),
//! `failed` (with why preservation or push did not happen), or `none` (no
//! candidate). Before the normal commit, dirty files are preserved as one
//! marked partial commit without moving the branch or changing the index or
//! files. Protected paths refuse the snapshot before any push. The hook
//! never fails the run further: every outcome is evidence for the owner,
//! which decides from it whether the task's next claim may resume the
//! candidate.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::text::floor_char_boundary;
use serde_json::{Value, json};

use crate::context::RuntimeHost;
use crate::executor::automation::input::{input_string_field, required_input_string};

use super::super::commit::commit_claimed_partial;
use super::super::git::git_output;
use super::super::operations::{CANDIDATE_REF_PREFIX, CANDIDATE_REF_PUSH};
use super::super::push::ensure_origin_publishes_elsewhere;

/// Largest push diagnostic the carry output keeps.
const MAX_CARRY_REASON_BYTES: usize = 1024;

/// Make the failed leaf's unpublished candidate fetchable from
/// `origin`, and report where it is.
pub(in crate::executor::automation) fn claim_candidate_carry<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
) -> Result<Value, OrbitError> {
    let run_id = required_input_string(input, "run_id")?;
    let step = |step: &str, field: &str| {
        input
            .get("pipeline")
            .and_then(|pipeline| pipeline.get(step))
            .and_then(|output| input_string_field(output, field))
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    };
    let task_ids = input
        .pointer("/job_input/task_ids")
        .and_then(Value::as_array)
        .map(|ids| ids.iter().filter_map(Value::as_str).collect::<Vec<_>>())
        .unwrap_or_default();
    let [task_id] = task_ids.as_slice() else {
        return Ok(none("the leaf names no single task"));
    };
    if let (Some(branch), Some(head_sha)) = (step("push", "branch"), step("push", "local_sha")) {
        return Ok(json!({
            "phase": "candidate_carry",
            "carry": "published",
            "task_id": task_id,
            "branch": branch,
            "head_sha": head_sha,
        }));
    }
    let Some(workspace_path) = step("worktree", "workspace_path") else {
        return Ok(none("the leaf recorded no worktree"));
    };
    let workspace_path = Path::new(&workspace_path);
    // The branch the leaf synchronized, or prepared, or set up: its tip is
    // the candidate, with any review fix committed after synchronization.
    // An interrupted rebase leaves the ref at its pre-rebase commit. A leaf
    // that continued an earlier claim's branch renamed its own to it
    // [ORB-15308].
    let Some(branch) = step("sync_base", "head")
        .or_else(|| step("prepare_branch", "head"))
        .or_else(|| step("resume_candidate", "reused_branch"))
        .or_else(|| step("worktree", "head_ref"))
    else {
        return Ok(none("the leaf recorded no candidate branch"));
    };
    let mut head_sha = match git_output(
        workspace_path,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}^{{commit}}"),
        ],
    ) {
        Ok(sha) if !sha.trim().is_empty() => sha.trim().to_string(),
        _ => return Ok(none(&format!("branch '{branch}' no longer resolves"))),
    };
    let partial = step("commit", "commit_sha").is_none();
    if partial {
        let failed_step = input_string_field(input, "failed_step_id").unwrap_or_default();
        let message = format!(
            "Partial implementation candidate [{task_id}]\n\nOrbit-Partial-Candidate-Run: {run_id}\n\
             Orbit-Failed-Step: {failed_step}"
        );
        match commit_claimed_partial(workspace_path, &message) {
            Ok(Some(commit)) => head_sha = commit,
            Ok(None) => return Ok(none("the leaf left no uncommitted candidate")),
            Err(error) => {
                return Ok(json!({
                    "phase": "candidate_carry",
                    "carry": "failed",
                    "task_id": task_id,
                    "reason": error.to_string(),
                }));
            }
        }
    }
    let mut output = json!({
        "phase": "candidate_carry",
        "task_id": task_id,
        "branch": branch,
        "head_sha": head_sha,
    });
    if partial {
        output["partial"] = json!(true);
    }
    match carry_to_durable_ref(host, workspace_path, &branch, &head_sha, task_id, run_id) {
        Ok(durable_ref) => {
            output["carry"] = json!("durable");
            output["durable_ref"] = json!(durable_ref);
        }
        Err(reason) => {
            tracing::warn!(
                run_id,
                task_id,
                head_sha,
                error = %reason,
                "claimed candidate could not be carried to a durable ref"
            );
            output["carry"] = json!("failed");
            output["reason"] = json!(reason);
        }
    }
    Ok(output)
}

/// Push the committed candidate `head_sha` on `branch` to
/// `refs/orbit/candidates/<task_id>/<run_id>` on `origin`, so a run on any
/// host can fetch it. Returns the ref, or the bounded push diagnostic.
/// [ORB-14905] An owner-local hold carries its candidate the same way.
pub(in crate::executor::automation::vcs) fn carry_to_durable_ref<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace_path: &Path,
    branch: &str,
    head_sha: &str,
    task_id: &str,
    run_id: &str,
) -> Result<String, String> {
    let durable_ref = format!("{CANDIDATE_REF_PREFIX}{task_id}/{run_id}");
    push_candidate_ref(host, workspace_path, branch, head_sha, &durable_ref).map(|()| durable_ref)
}

/// Push `commit`, made in the checkout of `branch`, to `target_ref` under
/// `refs/orbit/candidates/` on `origin`. The error is the bounded push
/// diagnostic.
pub(in crate::executor::automation::vcs) fn push_candidate_ref<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace_path: &Path,
    branch: &str,
    commit: &str,
    target_ref: &str,
) -> Result<(), String> {
    ensure_origin_publishes_elsewhere(workspace_path, branch)
        .and_then(|()| {
            host.run_private_vcs_operation(
                CANDIDATE_REF_PUSH,
                json!({
                    "repo_root": workspace_path.to_string_lossy(),
                    "head_sha": commit,
                    "target_ref": target_ref,
                }),
            )
        })
        .map(|_| ())
        .map_err(|error| {
            let reason = error.to_string();
            let reason = reason.trim();
            reason[..floor_char_boundary(reason, MAX_CARRY_REASON_BYTES)].to_string()
        })
}

fn none(reason: &str) -> Value {
    json!({
        "phase": "candidate_carry",
        "carry": "none",
        "reason": reason,
    })
}
