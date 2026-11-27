use std::path::Path;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::fs::selector::claim_new_path_is_safe;
use orbit_types::task::{TaskArtifact, TaskComment, TaskStatus};
use orbit_types::workflow::{
    CommitType, FailureHandoffDecision, REVIEW_PR_MERGED_OUTSIDE_ORBIT, ReviewGateAdmissionOutput,
};
use serde_json::{Value, json};

use crate::context::{
    HostEnvironment, ReviewGate, ReviewReleaseRequest, RunLifecycle, RuntimeHostProcessEnvironment,
    TaskAutomationUpdate,
};
use crate::executor::automation::input::{
    canonicalize_existing_dir, input_string_field, required_input_string,
};

use super::super::claim::carry_to_durable_ref;
use super::super::commit::{capture_uncommitted_diff, partial_repair_message};
use super::super::freshness::{commit_sha, rebase_belongs_to_attempt, rebase_provenance_summary};
use super::super::git::{BINARY_PATCH_DIFF_ARGS, git_output, git_output_bytes, git_output_paths};
use super::super::push::push_batch_changes_inner;
use super::super::resume::ensure_retry_descends_from;
use super::super::review_gate::{commit_reviewer_repairs, uncommitted_paths};
use super::conflict::{pipeline_checkpoint_string, pipeline_step};
use super::preserve::recorded_spec_digest;
use super::{FOREIGN_REBASE_EVENT, LANDING_REVIEW_STEPS, PipelineFailure, REVIEW_ADMISSION_STEPS};

/// The settlement step of the before-landing review.
const LANDING_SETTLE_STEP: &str = "landing_review_gate_settle";

/// The refusal a before-landing settlement leads with when the verdict is
/// `incomplete`.
const INCOMPLETE_VERDICT_REFUSAL: &str = "review_gate_blocked: verdict incomplete";

pub(super) fn ensure_failure_handoff_ownership<H: RunLifecycle + ?Sized>(
    host: &H,
    input: &Value,
    task: &orbit_types::task::Task,
    run_id: &str,
) -> Result<(), OrbitError> {
    let task_owner = task.job_run_id.as_deref().ok_or_else(|| {
        OrbitError::Execution(format!(
            "pr_failure_handoff: task '{}' has no owning run; refusing handoff for run '{}'",
            task.id, run_id
        ))
    })?;
    if task_owner == run_id {
        return Ok(());
    }

    let worktree = pipeline_step(input, "worktree")?;
    let checkpoint_owner = input_string_field(worktree, "job_run_id")
        .or_else(|| input_string_field(worktree, "batch_id"))
        .ok_or_else(|| {
            OrbitError::Execution(format!(
                "pr_failure_handoff: resumed run '{run_id}' has no worktree ownership checkpoint"
            ))
        })?;
    if task_owner != checkpoint_owner {
        return Err(OrbitError::Execution(format!(
            "pr_failure_handoff: task '{}' belongs to run '{}', not active run '{}' or worktree checkpoint owner '{}'",
            task.id, task_owner, run_id, checkpoint_owner
        )));
    }

    ensure_retry_descends_from(
        host,
        "pr_failure_handoff",
        "worktree checkpoint owner",
        &task.id,
        run_id,
        &checkpoint_owner,
    )
}

/// [ORB-15329] Push the reviewer commit a before-landing review recorded
/// before it settled `incomplete`, so it is not stranded in this host's
/// worktree.
///
/// Settlement commits the reviewer's repair before it judges the verdict, so
/// an `incomplete` refusal leaves that commit on the worktree branch and
/// `landing_push` never runs. It goes onto the published head the review
/// reviewed under a lease on it and nowhere else. This is a preservation
/// push: the commit is neither approved nor revalidated, the PR stays open
/// and unmerged, and `complete_pr` does not run. `None` when the failure is
/// not an incomplete settlement; otherwise the outcome, where a push that
/// could not be made is recorded rather than raised so the original failure
/// stays authoritative.
pub(super) fn push_incomplete_landing_reviewer_commit<
    H: RunLifecycle + ReviewGate + HostEnvironment + RuntimeHostProcessEnvironment + ?Sized,
>(
    host: &H,
    input: &Value,
    failed_step_id: &str,
    error_message: &str,
) -> Option<Value> {
    // A merged PR cannot incorporate the reviewer's unapproved commit.
    if failed_step_id != LANDING_SETTLE_STEP
        || !error_message.contains(INCOMPLETE_VERDICT_REFUSAL)
        || error_message.contains(REVIEW_PR_MERGED_OUTSIDE_ORBIT)
    {
        return None;
    }
    Some(
        push_landing_reviewer_commit(host, input)
            .unwrap_or_else(|error| json!({"decision": "push_failed", "error": error.to_string()})),
    )
}

fn push_landing_reviewer_commit<
    H: RunLifecycle + ReviewGate + HostEnvironment + RuntimeHostProcessEnvironment + ?Sized,
>(
    host: &H,
    input: &Value,
) -> Result<Value, OrbitError> {
    let worktree = pipeline_step(input, "worktree")?;
    let workspace_path = canonicalize_existing_dir(
        required_input_string(worktree, "workspace_path")?,
        "pipeline.worktree.workspace_path",
    )?;
    let missing = |step: &str, field: &str| {
        OrbitError::Execution(format!(
            "pr_failure_handoff: the run recorded no `{step}.{field}`"
        ))
    };
    let branch = pipeline_checkpoint_string(input, "sync_base", "head")
        .ok_or_else(|| missing("sync_base", "head"))?;
    let published = pipeline_checkpoint_string(input, "push", "local_sha")
        .ok_or_else(|| missing("push", "local_sha"))?;
    if commit_sha(&host.process_env(), &workspace_path, &branch)? == published {
        return Ok(json!({"decision": "no_reviewer_commit", "published_head_sha": published}));
    }
    let mut pushed = push_batch_changes_inner(
        host,
        &json!({
            "branch": branch,
            "workspace_path": workspace_path,
            "lease_remote_sha": published,
        }),
        &workspace_path,
    )?;
    pushed["published_head_sha"] = json!(published);
    Ok(pushed)
}

/// Preserve a non-accept before-landing review's fix commit or mid-repair
/// work onto `refs/orbit/candidates/<task>/<run>` (never pushing rejected or
/// unvalidated commits to the PR head branch), or save an uncommitted diff as
/// a task artifact when a commit/push fails.
pub(super) fn preserve_landing_reviewer_repair<
    H: HostEnvironment + RuntimeHostProcessEnvironment + ?Sized,
>(
    host: &H,
    task: &orbit_types::task::Task,
    run_id: &str,
    input: &Value,
    failed_step_id: &str,
    error_message: &str,
) -> Option<(Value, Option<TaskArtifact>)> {
    if !LANDING_REVIEW_STEPS.contains(&failed_step_id)
        || error_message.contains(REVIEW_PR_MERGED_OUTSIDE_ORBIT)
    {
        return None;
    }
    Some(preserve_landing_reviewer_repair_inner(
        host,
        task,
        run_id,
        input,
        failed_step_id,
    ))
}

fn preserve_landing_reviewer_repair_inner<
    H: HostEnvironment + RuntimeHostProcessEnvironment + ?Sized,
>(
    host: &H,
    task: &orbit_types::task::Task,
    run_id: &str,
    input: &Value,
    failed_step_id: &str,
) -> (Value, Option<TaskArtifact>) {
    let worktree = match pipeline_step(input, "worktree")
        .and_then(|step| required_input_string(step, "workspace_path"))
        .and_then(|raw| canonicalize_existing_dir(raw, "pipeline.worktree.workspace_path"))
    {
        Ok(path) => path,
        Err(error) => {
            return (
                json!({"decision": "unavailable", "error": error.to_string()}),
                None,
            );
        }
    };
    let branch = pipeline_checkpoint_string(input, "sync_base", "head").or_else(|| {
        git_output(
            &host.process_env(),
            &worktree,
            &["branch", "--show-current"],
        )
        .ok()
        .filter(|name| !name.is_empty())
    });
    let admission = pipeline_step(input, "landing_review_gate_admit")
        .ok()
        .and_then(|output| ReviewGateAdmissionOutput::from_value(output).ok());
    let admitted = admission
        .as_ref()
        .and_then(ReviewGateAdmissionOutput::admitted);
    let Some(published) = pipeline_checkpoint_string(input, "push", "local_sha")
        .or_else(|| admitted.map(|entry| entry.head_sha.clone()))
        .filter(|sha| !sha.is_empty())
    else {
        return (
            json!({
                "decision": "unavailable",
                "error": "pr_failure_handoff: the run recorded no `push.local_sha`",
            }),
            None,
        );
    };

    let mut committed_by_handoff = false;
    if failed_step_id == "landing_review"
        && let Ok(uncommitted) = uncommitted_paths(&host.process_env(), &worktree)
        && !uncommitted.is_empty()
    {
        let reviewer_model = admitted
            .map(|entry| entry.reviewer.model.trim())
            .filter(|model| !model.is_empty())
            .unwrap_or("orbit");
        let attempt_id = admitted
            .map(|entry| entry.attempt_id.trim())
            .filter(|id| !id.is_empty());
        let commit_type = CommitType::for_bundle(std::iter::once(task.task_type));
        let message =
            partial_repair_message(commit_type, &task.id, run_id, "landing_review", attempt_id);
        match commit_reviewer_repairs(&host.process_env(), &worktree, reviewer_model, &message) {
            Ok(Some(_)) => committed_by_handoff = true,
            Ok(None) => {}
            Err(commit_error) => match capture_uncommitted_diff(&host.process_env(), &worktree) {
                Ok(Some(diff)) => {
                    let artifact_path = format!("landing-review-uncommitted-{run_id}.patch");
                    return (
                        json!({
                            "decision": "commit_refused",
                            "published_head_sha": published,
                            "error": commit_error.to_string(),
                            "diff_artifact": artifact_path,
                        }),
                        Some(TaskArtifact::from_bytes(artifact_path, diff)),
                    );
                }
                Ok(None) => {
                    return (
                        json!({
                            "decision": "commit_refused",
                            "published_head_sha": published,
                            "error": commit_error.to_string(),
                        }),
                        None,
                    );
                }
                Err(diff_error) => {
                    return (
                        json!({
                            "decision": "commit_refused",
                            "published_head_sha": published,
                            "error": commit_error.to_string(),
                            "diff_error": diff_error.to_string(),
                        }),
                        None,
                    );
                }
            },
        }
    }

    let head_sha = match branch.as_deref() {
        Some(branch_name) => {
            commit_sha(&host.process_env(), &worktree, branch_name).or_else(|_| {
                git_output(
                    &host.process_env(),
                    &worktree,
                    &["rev-parse", "--verify", "HEAD^{commit}"],
                )
            })
        }
        None => git_output(
            &host.process_env(),
            &worktree,
            &["rev-parse", "--verify", "HEAD^{commit}"],
        ),
    };
    let head_sha = match head_sha {
        Ok(sha) => sha,
        Err(error) => {
            return (
                json!({
                    "decision": "unavailable",
                    "published_head_sha": published,
                    "error": error.to_string(),
                }),
                None,
            );
        }
    };
    if head_sha == published {
        return (
            json!({
                "decision": "no_reviewer_commit",
                "published_head_sha": published,
            }),
            None,
        );
    }

    match carry_to_durable_ref(
        host,
        &worktree,
        branch.as_deref().unwrap_or(""),
        &head_sha,
        &task.id,
        run_id,
    ) {
        Ok(durable_ref) => (
            json!({
                "decision": "preserved",
                "commit_sha": head_sha,
                "published_head_sha": published,
                "ref": durable_ref,
                "committed_by_handoff": committed_by_handoff,
            }),
            None,
        ),
        Err(error) => {
            // Disable rename detection for both the inventory and patch: each
            // endpoint must pass the path guard independently, including deletions.
            let patch = git_output_paths(
                &host.process_env(),
                &worktree,
                &[
                    "diff",
                    "--no-renames",
                    "--name-only",
                    "-z",
                    &published,
                    &head_sha,
                ],
            )
            .ok()
            .and_then(|paths| {
                let (safe, protected): (Vec<String>, Vec<String>) = paths
                    .into_iter()
                    .partition(|path| claim_new_path_is_safe(path));
                if safe.is_empty() {
                    return None;
                }
                // A literal pathspec also matches descendants. A file that
                // replaces a published directory (or the reverse) would pull
                // its protected descendants back in, so exclude them by name.
                let pathspecs = safe
                    .iter()
                    .map(|path| format!(":(literal){path}"))
                    .chain(
                        protected
                            .iter()
                            .map(|path| format!(":(exclude,literal){path}")),
                    )
                    .collect::<Vec<_>>();
                let mut args = vec!["diff"];
                args.extend_from_slice(BINARY_PATCH_DIFF_ARGS);
                args.extend([published.as_str(), head_sha.as_str(), "--"]);
                args.extend(pathspecs.iter().map(String::as_str));
                git_output_bytes(&host.process_env(), &worktree, &args).ok()
            })
            .filter(|diff| !diff.iter().all(u8::is_ascii_whitespace))
            .map(|diff| orbit_common::security::redaction::redact_all_bytes(&diff));
            if let Some(diff) = patch {
                let artifact_path = format!("landing-review-fix-{run_id}.patch");
                (
                    json!({
                        "decision": "preserve_failed",
                        "commit_sha": head_sha,
                        "published_head_sha": published,
                        "error": error,
                        "diff_artifact": artifact_path,
                    }),
                    Some(TaskArtifact::from_bytes(artifact_path, diff)),
                )
            } else {
                (
                    json!({
                        "decision": "preserve_failed",
                        "commit_sha": head_sha,
                        "published_head_sha": published,
                        "error": error,
                    }),
                    None,
                )
            }
        }
    }
}

pub(super) fn preserve_completion_failure<H: RunLifecycle + ?Sized>(
    host: &H,
    task: &orbit_types::task::Task,
    failure: PipelineFailure<'_>,
    pr_number: &str,
    reviewer_push: Option<&Value>,
    reviewer_preservation: Option<&Value>,
    preservation_artifact: Option<TaskArtifact>,
) -> Result<Value, OrbitError> {
    let PipelineFailure {
        run_id,
        failed_step_id,
        error_code,
        error_message,
    } = failure;
    let pr_url = task
        .external_refs
        .iter()
        .find(|external_ref| external_ref.system == "github-pr" && external_ref.id == pr_number)
        .and_then(|external_ref| external_ref.url.clone());
    // [ORB-14849] A before-landing review that did not approve settles here
    // too: its PR stays open and unmerged, under the step's typed reason.
    let landing_review = LANDING_REVIEW_STEPS
        .contains(&failed_step_id)
        .then(|| typed_reason(error_code, error_message));
    let merged_outside_orbit =
        landing_review.is_some() && error_message.contains(REVIEW_PR_MERGED_OUTSIDE_ORBIT);
    let note = if merged_outside_orbit {
        format!(
            "Before-landing review ended because the pull request had already merged; inspect `orbit run show {run_id} --json` for details."
        )
    } else if landing_review.is_some() {
        let reason = match orbit_types::workflow::ReviewGateRefusal::from_error_code(error_code) {
            Some(orbit_types::workflow::ReviewGateRefusal::TimeoutIncomplete) => {
                "the reviewer timed out"
            }
            Some(orbit_types::workflow::ReviewGateRefusal::AwaitingEvidence) => {
                "review evidence is missing"
            }
            Some(orbit_types::workflow::ReviewGateRefusal::Abandoned) => {
                "the reviewer left without a verdict"
            }
            Some(orbit_types::workflow::ReviewGateRefusal::Blocked) => "the review did not approve",
            None if failed_step_id == "landing_review_validate" => {
                "reviewer fixes failed validation"
            }
            None => "a review step failed",
        };
        format!(
            "Before-landing review stopped because {reason}; inspect `orbit run show {run_id} --json` before deciding what to do next."
        )
    } else {
        format!(
            "Pull request completion stopped because a completion step failed; inspect `orbit run show {run_id} --json` before retrying."
        )
    };
    host.apply_task_automation_update(
        &task.id,
        TaskAutomationUpdate {
            append_comments: vec![TaskComment {
                at: Utc::now(),
                by: "system".to_string(),
                message: note,
                operation_id: None,
            }],
            upsert_artifacts: preservation_artifact.into_iter().collect(),
            ..TaskAutomationUpdate::default()
        },
    )?;

    Ok(json!({
        "phase": "failure_handoff",
        "decision": if landing_review.is_some() {
            FailureHandoffDecision::LandingReviewFailure.as_str()
        } else {
            FailureHandoffDecision::ReviewCompletionFailure.as_str()
        },
        "reason": landing_review,
        "reviewer_push": reviewer_push,
        "reviewer_preservation": reviewer_preservation,
        // An operator requeue of the task this run's final recovery blocked
        // resumes the kept PR only for the run and specification named here.
        "task_id": task.id,
        "handoff_run_id": run_id,
        "task_spec_digest": recorded_spec_digest(host, &task.id)?,
        "failed_step_id": failed_step_id,
        "pr_number": pr_number,
        "pr_url": pr_url,
        "candidate_preserved": true,
        "task_status": task.status.to_string(),
    }))
}

/// The typed reason for a failed step: a review-gate refusal's code, else the
/// first `snake_case:` token its message leads with, such as
/// `push_lease_lost`, else the step's error code.
fn typed_reason(error_code: &str, error_message: &str) -> String {
    if let Some(refusal) = orbit_types::workflow::ReviewGateRefusal::from_error_code(error_code) {
        return refusal.error_code().to_string();
    }
    error_message
        .split_whitespace()
        .filter_map(|word| word.strip_suffix(':'))
        .find(|word| {
            word.contains('_')
                && word
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        })
        .unwrap_or(error_code)
        .to_string()
}

/// Whether the in-progress rebase is the one `sync_base` started from this
/// run's `prepare_branch` checkpoint: its `orig-head`, `onto`, and
/// `head-name` must name the prepared head SHA, base SHA, and branch. Without
/// that checkpoint no Orbit step started a rebase, so none is owned.
pub(super) fn prepared_attempt_owns_rebase(
    process_env: &orbit_common::Env,
    input: &Value,
    workspace_path: &Path,
) -> Result<bool, OrbitError> {
    let (Some(head), Some(head_sha), Some(base_sha)) = (
        pipeline_checkpoint_string(input, "prepare_branch", "head"),
        pipeline_checkpoint_string(input, "prepare_branch", "head_sha"),
        pipeline_checkpoint_string(input, "prepare_branch", "base_sha"),
    ) else {
        return Ok(false);
    };
    rebase_belongs_to_attempt(process_env, workspace_path, &head, &head_sha, &base_sha)
}

/// Leave a rebase this run did not start untouched: no abort, commit, push,
/// or PR. The task is blocked with the rebase provenance so an operator can
/// inspect the worktree; the original step error stays authoritative.
pub(super) fn refuse_foreign_rebase<H: RunLifecycle + RuntimeHostProcessEnvironment + ?Sized>(
    host: &H,
    task: &orbit_types::task::Task,
    run_id: &str,
    failed_step_id: &str,
    _error_code: &str,
    _error_message: &str,
    workspace_path: &Path,
) -> Result<Value, OrbitError> {
    let provenance = rebase_provenance_summary(&host.process_env(), workspace_path);
    let note = format!(
        "failure handoff refused a rebase this run did not start: run={run_id}, \
         failed_step={failed_step_id}, {provenance}; nothing was aborted, committed, pushed, \
         or published"
    );
    let message = format!(
        "Delivery stopped because an unrelated rebase is in progress; inspect `orbit run show {run_id} --json` before retrying."
    );
    host.apply_task_automation_update(
        &task.id,
        TaskAutomationUpdate {
            status: Some(TaskStatus::Blocked),
            status_event: Some(FOREIGN_REBASE_EVENT.to_string()),
            status_note: Some(note),
            append_comments: vec![TaskComment {
                at: Utc::now(),
                by: "system".to_string(),
                message,
                operation_id: None,
            }],
            ..TaskAutomationUpdate::default()
        },
    )?;

    Ok(json!({
        "phase": "failure_handoff",
        "decision": FailureHandoffDecision::ForeignRebaseRefused.as_str(),
        "task_id": task.id,
        "handoff_run_id": run_id,
        "failed_step_id": failed_step_id,
        "workspace_path": workspace_path,
        "rebase_provenance": provenance,
        "pr_created": false,
        "task_status": "blocked",
    }))
}

/// [ORB-13890] Close every review attempt this run admitted that has no
/// verdict yet, charging the reviewer runtime it spent, so a failed or
/// timed-out reviewer step never leaves its attempt open for the lineage.
/// Only the run's own admission checkpoints are read, so this needs no task
/// ownership proof. The original failure stays authoritative: a release
/// error is logged, not raised; the run's termination releases it again.
pub(super) fn release_review_attempts<H: ReviewGate + ?Sized>(
    host: &H,
    input: &Value,
    run_id: &str,
) {
    for step in REVIEW_ADMISSION_STEPS {
        let Some(admission) = pipeline_step(input, step)
            .ok()
            .and_then(|output| ReviewGateAdmissionOutput::from_value(output).ok())
        else {
            continue;
        };
        let Some(admitted) = admission.admitted() else {
            continue;
        };
        let (lineage_key, attempt_id) = (admitted.lineage_key.trim(), admitted.attempt_id.trim());
        if lineage_key.is_empty() || attempt_id.is_empty() {
            continue;
        }
        let request = ReviewReleaseRequest {
            run_id: run_id.to_string(),
            lineage_key: lineage_key.to_string(),
            attempt_id: attempt_id.to_string(),
        };
        if let Err(error) = host.release_review_attempt(&request) {
            tracing::warn!(
                run_id = %run_id,
                attempt_id = %request.attempt_id,
                error = %error,
                "pr_failure_handoff could not release the review attempt; run termination releases it"
            );
        }
    }
}
