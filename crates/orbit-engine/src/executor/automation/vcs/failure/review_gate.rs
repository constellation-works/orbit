use std::path::Path;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_types::task::{TaskComment, TaskStatus};
use orbit_types::workflow::REVIEW_ABANDONED_MARKER;
use serde_json::{Value, json};

use crate::context::{RuntimeHost, TaskAutomationUpdate};
use crate::executor::automation::input::input_string_field;

use super::super::freshness::remote_branch_sha;
use super::super::git::{git_output, git_output_raw};
use super::super::push::push_batch_changes_inner;
use super::conflict::pipeline_checkpoint_string;
use super::preserve::recorded_spec_digest;
use super::{REVIEW_GATE_EVENT, REVIEW_VALIDATION_STEP};

/// A fresh run resets its review ledger; bound automatic continuations across
/// those lineages by the preserved tree instead of the rewritten commit.
const REVIEW_TIMEOUT_MAX_REQUEUES: usize = 1;

/// Subject of the commit that preserves a timed-out reviewer's uncommitted edits.
const PARTIAL_REPAIR_SUBJECT: &str = "review: partial reviewer repairs preserved";

/// Trailer that marks a partial-repair commit as the gate's own.
const PARTIAL_REPAIR_RUN_TRAILER: &str = "Orbit-Review-Run:";

/// Preserve a candidate the before-PR review gate refused to publish.
///
/// Uncommitted reviewer changes are committed under the reviewer identity the
/// gate admitted, never as implementer work; the implementation and reviewer
/// commits stay as they are; the branch is pushed so partial fixes and
/// evidence are recoverable. The task records the bounded timeout requeue,
/// evidence hold or substantive escalation. No PR is opened.
#[allow(clippy::too_many_arguments)]
pub(super) fn preserve_review_gate_candidate<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
    task: &orbit_types::task::Task,
    run_id: &str,
    failed_step_id: &str,
    error_code: &str,
    error_message: &str,
    workspace_path: &Path,
) -> Result<Value, OrbitError> {
    let reviewer = input
        .get("pipeline")
        .and_then(|pipeline| pipeline.get("review_gate_admit"))
        .and_then(|admit| admit.get("reviewer"));
    let reviewer_model = reviewer.and_then(|reviewer| {
        let provider = reviewer.get("provider")?.as_str()?.trim();
        let model = reviewer.get("model")?.as_str()?.trim();
        (!provider.is_empty() && !model.is_empty()).then(|| format!("{provider} / {model}"))
    });
    let attempt_id = input
        .get("pipeline")
        .and_then(|pipeline| pipeline.get("review_gate_admit"))
        .and_then(|admit| input_string_field(admit, "attempt_id"));
    let attempt_trailer = attempt_id
        .map(|id| format!("\nOrbit-Review-Attempt: {id}"))
        .unwrap_or_default();
    let partial_repair = match &reviewer_model {
        Some(model) => super::super::review_gate::commit_reviewer_repairs(
            workspace_path,
            model,
            &format!(
                "{PARTIAL_REPAIR_SUBJECT} [{}]\n\n{PARTIAL_REPAIR_RUN_TRAILER} {run_id}\n\
                 Orbit-Review-Step: {failed_step_id}{attempt_trailer}",
                task.id
            ),
        )?,
        None => None,
    };
    let leftover = super::super::review_gate::uncommitted_paths(workspace_path)?;
    let head = git_output(workspace_path, &["rev-parse", "--abbrev-ref", "HEAD"])?
        .trim()
        .to_string();
    if head == "HEAD" {
        return Err(OrbitError::Execution(
            "pr_failure_handoff: review-gate candidate is detached".to_string(),
        ));
    }
    let head_sha = git_output(workspace_path, &["rev-parse", "--verify", "HEAD^{commit}"])?;
    let candidate_tree = git_output(workspace_path, &["rev-parse", "--verify", "HEAD^{tree}"])?;
    let implementation_tree = implementation_tree(workspace_path)?;
    let pushed = push_batch_changes_inner(
        host,
        &review_gate_preservation_push_input(input, &head, workspace_path, &head_sha)?,
        workspace_path,
    )?;

    // Only admission reaches this handoff with a held-evidence refusal:
    // settlement ends the run as a held outcome before any failure handoff.
    // Admission refuses before it reserves an attempt, and a requeued task is
    // admitted by a later run. Its refusal already proved that the held
    // candidate's tree and task meaning still match and that the evidence has
    // not arrived, so the admission path checks only the candidate commit.
    let evidence_hold = task.status == TaskStatus::InProgress
        && failed_step_id == "review_gate_admit"
        && error_message.contains("review_awaiting_evidence:")
        && host.get_task_artifacts(&task.id)?.iter().any(|artifact| {
            artifact.path == orbit_types::workflow::REVIEW_EVIDENCE_HOLD_ARTIFACT
                && artifact.created_by.as_deref() == Some("system")
                && serde_json::from_slice::<orbit_types::workflow::ReviewEvidenceHold>(
                    &artifact.content,
                )
                .is_ok_and(|hold| {
                    !hold.requirements.is_empty() && hold.candidate.commit == head_sha
                })
        });
    let timed_out = task.status == TaskStatus::InProgress
        && failed_step_id == "review"
        && error_message.contains("review_timeout_incomplete:");
    let timeout_requeues = if timed_out {
        host.get_task_history(&task.id)?
            .iter()
            .filter(|entry| {
                entry.event == "review_timeout_incomplete"
                    && entry.note.as_deref().is_some_and(|note| {
                        recorded_implementation_tree(note) == Some(implementation_tree.as_str())
                    })
            })
            .count()
    } else {
        0
    };
    let timeout_exhausted = timed_out && timeout_requeues >= REVIEW_TIMEOUT_MAX_REQUEUES;
    // [ORB-15130] Settlement released an attempt whose reviewer ended with only
    // its initial placeholder report: no verdict, so no escalation to decide.
    let abandoned =
        failed_step_id == "review_gate_settle" && error_message.contains(REVIEW_ABANDONED_MARKER);
    let (status, event, decision) = if evidence_hold {
        (
            TaskStatus::InProgress,
            "review_awaiting_evidence",
            "awaiting_review_evidence",
        )
    } else if timeout_exhausted {
        (
            TaskStatus::Blocked,
            "review_timeout_requeue_exhausted",
            "blocked_review_gate",
        )
    } else if timed_out {
        (
            TaskStatus::Backlog,
            "review_timeout_incomplete",
            "incomplete_review_timeout",
        )
    } else {
        (
            TaskStatus::Blocked,
            REVIEW_GATE_EVENT,
            "blocked_review_gate",
        )
    };
    let continuation = if evidence_hold {
        "Awaiting named external evidence. Attach each matching passing result and its log to the task; once all checks arrive, delivery is requeued for review.".to_string()
    } else if timeout_exhausted {
        format!(
            "Reviewer timed out again on the same implementation tree; automatic timeout requeue limit exhausted ({timeout_requeues}/{REVIEW_TIMEOUT_MAX_REQUEUES}). Delivery is blocked. Repair the candidate or record an operator decision before continuing; the partial report is retained."
        )
    } else if timed_out {
        format!(
            "Reviewer timed out and settled incomplete. The partial report is retained; delivery is requeued ({}/{REVIEW_TIMEOUT_MAX_REQUEUES} automatic timeout requeues for this implementation tree). A fresh run starts a new review lineage with its captured budget; resuming the same lineage uses its remaining budget.",
            timeout_requeues + 1,
        )
    } else if abandoned {
        "The reviewer ended its session with only its initial placeholder report, so the review produced no verdict. The attempt was released and the candidate's review is not spent. Delivery is blocked until a reviewer is admitted again by resuming the run or requeueing the task; it runs within the review's remaining minutes."
            .to_string()
    } else {
        "A substantive review escalation requires a recorded repair or scope decision.".to_string()
    };
    let note = format!(
        "before-PR review gate stopped delivery: run={run_id}, failed_step={failed_step_id}, \
         candidate={head_sha}, candidate_tree={candidate_tree}, implementation_tree={implementation_tree}, branch={head}; no PR was opened; {continuation}"
    );
    let verdict = if failed_step_id == REVIEW_VALIDATION_STEP {
        "Verdict: `reject`. The reviewer's fixes did not pass owner revalidation (required \
         validation or path ownership) on the reviewed head, and there is no second review \
         round.\n\n"
    } else {
        ""
    };
    let body = format!(
        "## Review gate escalation\n\nOrbit held PR publication because the before-PR review gate \
         did not pass. {verdict}The candidate branch was pushed so the implementation commit, \
         any reviewer commit, and the review evidence remain inspectable; nothing was merged or \
         published as a PR.\n\n- Task: `{}`\n- Run: `{run_id}`\n- Failed step: `{failed_step_id}`\n\
         - Error code: `{error_code}`\n- Candidate branch: `{head}`\n- Candidate head: `{head_sha}`\n\
         - Partial reviewer commit: {}\n\
         - Uncommitted paths left in the worktree: {}\n\n\
         The review's verdict, findings and what changed for each are recorded in the gate's \
         settlement comment on the task. Continuation follows the timeout, evidence, or \
         substantive decision recorded with this handoff.\n\n\
         ## Failure\n\n```text\n{error_message}\n```",
        task.id,
        partial_repair
            .as_ref()
            .map(|commit| format!("`{}` ({})", commit.commit, commit.author))
            .unwrap_or_else(|| "none".to_string()),
        if leftover.is_empty() {
            "none".to_string()
        } else {
            leftover.join(", ")
        },
    );
    host.apply_task_automation_update(
        &task.id,
        TaskAutomationUpdate {
            expected_status: Some(task.status),
            status: Some(status),
            status_event: Some(event.to_string()),
            status_note: Some(note.clone()),
            append_comments: vec![TaskComment {
                at: Utc::now(),
                by: "system".to_string(),
                message: format!("{note}\n\n{body}"),
            }],
            ..TaskAutomationUpdate::default()
        },
    )?;

    Ok(json!({
        "phase": "failure_handoff",
        "decision": decision,
        "task_id": task.id,
        "handoff_run_id": run_id,
        "failed_step_id": failed_step_id,
        "branch": head,
        "head_sha": head_sha,
        "candidate_tree": candidate_tree,
        "implementation_tree": implementation_tree,
        "partial_repair_commit": partial_repair.map(|commit| commit.commit),
        "uncommitted_paths": leftover,
        "push": pushed,
        "pr_created": false,
        "task_status": status.to_string(),
        "task_spec_digest": recorded_spec_digest(host, &task.id)?,
    }))
}

/// The tree of the implementation commit under the candidate head.
///
/// Walks back over the gate's own partial-repair commits, which stack when a
/// requeued run builds on a preserved candidate and times out again. The
/// timeout bound is keyed on this tree: the reviewer's partial work changes
/// the candidate tree on every timeout and must not renew the allowance.
fn implementation_tree(workspace_path: &Path) -> Result<String, OrbitError> {
    let mut commit = git_output(workspace_path, &["rev-parse", "--verify", "HEAD^{commit}"])?;
    loop {
        let message = git_output_raw(
            workspace_path,
            &["log", "-1", "--format=%B", "--end-of-options", &commit],
        )?;
        let is_partial_repair = message
            .lines()
            .next()
            .is_some_and(|subject| subject.starts_with(PARTIAL_REPAIR_SUBJECT))
            && message
                .lines()
                .any(|line| line.starts_with(PARTIAL_REPAIR_RUN_TRAILER));
        if !is_partial_repair {
            break;
        }
        let lineage = git_output(
            workspace_path,
            &["rev-list", "--parents", "-n", "1", &commit],
        )?;
        match lineage.split_whitespace().nth(1) {
            Some(parent) => commit = parent.to_string(),
            None => break,
        }
    }
    git_output(
        workspace_path,
        &["rev-parse", "--verify", &format!("{commit}^{{tree}}")],
    )
}

/// The tree a recorded timeout counted against. Rows written before the
/// implementation tree was recorded carry only `candidate_tree=`.
fn recorded_implementation_tree(note: &str) -> Option<&str> {
    let field = |key: &str| note.split(", ").find_map(|field| field.strip_prefix(key));
    field("implementation_tree=").or_else(|| field("candidate_tree="))
}

/// Push input for a review-gate preservation.
///
/// First-time (missing origin) and fast-forward pushes ignore the lease
/// fields. A diverged origin is replaced only when the lease names the exact
/// remote SHA `git_push` currently observes. `rewrite_performed` is set by
/// this handoff even when this run's `sync_base` did not rewrite: a previous
/// preservation (or re-implementation onto the same head) still has to replace
/// the published candidate.
fn review_gate_preservation_push_input(
    input: &Value,
    branch: &str,
    workspace_path: &Path,
    local_sha: &str,
) -> Result<Value, OrbitError> {
    let mut push_input = json!({
        "branch": branch,
        "workspace_path": workspace_path,
    });
    if let Some((head_before, expected_remote_sha)) =
        review_gate_rewrite_lease(input, branch, workspace_path, local_sha)?
    {
        push_input["rewrite_performed"] = json!(true);
        push_input["rewrite_head_before"] = json!(head_before);
        push_input["expected_remote_sha"] = json!(expected_remote_sha);
    }
    Ok(push_input)
}

fn review_gate_rewrite_lease(
    input: &Value,
    branch: &str,
    workspace_path: &Path,
    local_sha: &str,
) -> Result<Option<(String, String)>, OrbitError> {
    let checkpointed_remote = pipeline_checkpoint_string(input, "sync_base", "remote_sha_before")
        .or_else(|| pipeline_checkpoint_string(input, "prepare_branch", "remote_sha"));
    let expected_remote_sha = match checkpointed_remote {
        Some(sha) => sha,
        None => match remote_branch_sha(workspace_path, branch)? {
            Some(sha) => sha,
            None => return Ok(None),
        },
    };

    let head_before = pipeline_checkpoint_string(input, "sync_base", "head_sha_before")
        .filter(|sha| sha != local_sha)
        .or_else(|| (expected_remote_sha != local_sha).then(|| expected_remote_sha.clone()));
    Ok(head_before.map(|head_before| (head_before, expected_remote_sha)))
}
