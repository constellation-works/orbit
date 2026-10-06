//! Resume a task's preserved candidate instead of re-implementing it
//! [ORB-13985].
//!
//! A task PR run that fails leaves its candidate on an `orbit/<task>-<hash>`
//! branch, and `pr_failure_handoff` records the branch, head, failed step and
//! the task's spec digest on that run. When the task runs again,
//! `candidate_resume` finds that record through the run the task was last
//! linked to and applies the candidate onto the new run's base as
//! uncommitted changes — a squash merge, so the run's own commit step
//! delivers it under the usual gates. Then:
//!
//! - it applies cleanly, the failed step is `commit` or later, and owner
//!   validation passes: `resumed_validated`, and no implementation step runs;
//! - the failed step is the implementation (`implement_bundle` /
//!   `implement_one`) or any step before `commit`: `resumed_repaired`, and
//!   the implementer finishes the applied partial candidate. Validation is
//!   not consulted, so an empty command list cannot accept it;
//! - it conflicts, validation fails, or the before-PR review refused it:
//!   `resumed_repaired`, and the implementer starts from the applied
//!   candidate with that output;
//! - validation could not run for lack of a tool, or fails exactly as it does
//!   on the base [ORB-14258]: `resumed_unjudged`, and no implementation step
//!   runs; the delivery's own validation decides;
//! - there is no usable candidate (none preserved, an operator discarded it,
//!   the spec changed, a bundle, the commit is gone): `fresh`, with the
//!   reason.
//!
//! Whenever a candidate was found, the outcome, source run and SHA are also
//! written to the task's history.
//!
//! A claimed leaf (`claimed: true`), PR or owner-local, resumes the candidate
//! its owner kept from the task's last claim [ORB-14257] [ORB-14338] instead,
//! handed in as `candidate`: the owner already retired a discarded one, one
//! whose spec changed and one this host cannot fetch, recording why in the
//! task's history, which is the owner's, so none of that is consulted here.
//! A candidate absent from this object store is fetched from `origin` — from
//! the durable ref its leaf carried it to (`durable_ref`), else its branch.
//! The claimed implementer always runs, because the handoff carries its
//! summary: a clean apply is `resumed_repaired` with trigger `continuation`
//! (or `review` when the before-PR review refused it), and the leaf's own
//! validation judges the result.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_types::task::{CANDIDATE_DISCARDED_EVENT, CANDIDATE_RESUME_EVENT, Task};
use serde_json::{Value, json};

use crate::context::{RuntimeHost, TaskAutomationUpdate};
use crate::executor::automation::input::{
    canonicalize_existing_dir, input_string_field, required_input_string, required_job_run_id,
};

use super::baseline::{compare_with_base, run_validation_command};
use super::git::{git_command_success, git_output, git_run, git_success};
use super::operations::valid_candidate_ref;

/// `pr_failure_handoff` decisions that leave a candidate on a branch.
const PRESERVING_DECISIONS: &[&str] = &[
    "blocked_failure_pr",
    "blocked_conflict_pr",
    "blocked_review_gate",
    "awaiting_review_evidence",
    "incomplete_review_timeout",
    "blocked_validation_environment",
    "held_baseline_red",
    "held_provider_failure",
];
/// The settlement step whose failure is the review's verdict on the
/// candidate, not a fault: the repair starts from its findings.
const REVIEW_VERDICT_STEP: &str = "review_gate_settle";
/// Steps of `task_pr_pipeline` and `task_local_pipeline` that run only after
/// `implement_bundle` has finished. A preserved candidate from one of these
/// is a completed implementation and may resume as `resumed_validated`.
///
/// Any other id — `implement_bundle`, the nested `implement_one`, a step
/// before `commit`, or a step this list does not name yet — is unfinished.
/// Unknown ids fail closed so a new pre-commit step cannot skip the
/// implementer. A new step after `commit` belongs here; until it is added,
/// resume hands that candidate to the implementer.
const COMPLETED_IMPLEMENTATION_STEPS: &[&str] = &[
    "commit",
    "prepare_branch",
    "sync_base",
    "validate",
    "review_gate_admit",
    "review",
    "review_gate_settle",
    "review_validate",
    "push",
    "pr_open",
    "promote_tasks",
    "promote_no_diff",
    "complete_pr",
    "re_review_gate_admit",
    "re_review",
    "re_review_gate_settle",
    "re_review_validate",
    "re_push",
    "complete_reviewed_pr",
    "re_review_gate_admit_2",
    "re_review_2",
    "re_review_gate_settle_2",
    "re_review_validate_2",
    "re_push_2",
    "complete_reviewed_pr_2",
    "complete_no_diff",
    "merge",
    "mark_review",
    "mark_review_one",
    "complete_tasks",
    "complete_one",
];
/// Largest failure output handed to the implementer; the tail is kept, where
/// compilers and test runners report.
const MAX_REPAIR_OUTPUT_BYTES: usize = 32 * 1024;

/// The candidate the task's last run preserved.
struct Candidate {
    run_id: String,
    branch: String,
    head_sha: String,
    /// [ORB-14338] The ref on `origin` a claimed leaf carried the candidate
    /// to, fetched in place of its branch.
    durable_ref: Option<String>,
    failed_step_id: String,
    needs_review_repair: bool,
}

/// What the task's last run left to resume.
enum Preserved {
    /// No candidate, and why.
    None(String),
    /// A candidate that must not be resumed, and why.
    Refused(Candidate, String),
    Usable(Candidate),
}

enum Outcome {
    Fresh(String),
    Validated,
    Unjudged(String),
    Repair(Value),
}

/// Apply the task's preserved candidate onto this run's base, if it may be
/// resumed, and decide whether the implementation step runs.
pub(in crate::executor::automation) fn candidate_resume<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
) -> Result<Value, OrbitError> {
    let run_id = required_job_run_id(input, "candidate_resume")?.to_string();
    let workspace_path = canonicalize_existing_dir(
        required_input_string(input, "workspace_path")?,
        "workspace_path",
    )?;
    let base_sha = required_input_string(input, "base_sha")?.to_string();
    let task_ids = input
        .get("task_ids")
        .and_then(Value::as_array)
        .map(|ids| ids.iter().filter_map(Value::as_str).collect::<Vec<_>>())
        .unwrap_or_default();
    let [task_id] = task_ids.as_slice() else {
        return Ok(output(
            &Outcome::Fresh("a bundle run implements every task fresh".to_string()),
            None,
            &base_sha,
        ));
    };
    if input.get("claimed").and_then(Value::as_bool) == Some(true) {
        return claimed_resume(host, input, task_id, &workspace_path, &base_sha);
    }
    let task = host.get_task(task_id)?;
    let prior_run_id = input_string_field(input, "prior_job_run_id");
    let (candidate, outcome) = match preserved_candidate(host, &task, prior_run_id)? {
        Preserved::None(reason) => return Ok(output(&Outcome::Fresh(reason), None, &base_sha)),
        Preserved::Refused(candidate, reason) => (candidate, Outcome::Fresh(reason)),
        Preserved::Usable(candidate) => {
            let outcome = resume(
                host,
                &task.id,
                &candidate,
                &workspace_path,
                &base_sha,
                false,
            )?;
            (candidate, outcome)
        }
    };
    record(host, &task, &run_id, &candidate, &outcome)?;
    Ok(output(&outcome, Some(&candidate), &base_sha))
}

/// [ORB-14257] Resume the candidate the owner kept from the task's last
/// claim. Nothing is written to the task: its history lives on the owner.
fn claimed_resume<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
    task_id: &str,
    workspace_path: &Path,
    base_sha: &str,
) -> Result<Value, OrbitError> {
    let preserved = input.get("candidate").filter(|value| !value.is_null());
    let (Some(branch), Some(head_sha)) = (
        preserved.and_then(|value| input_string_field(value, "branch")),
        preserved.and_then(|value| input_string_field(value, "head_sha")),
    ) else {
        return Ok(output(
            &Outcome::Fresh("the claim carries no preserved candidate".to_string()),
            None,
            base_sha,
        ));
    };
    let preserved = preserved.unwrap_or(&Value::Null);
    // A leaf that stopped after its last delivery step names none; its
    // candidate is complete.
    let failed_step_id =
        input_string_field(preserved, "failed_step_id").unwrap_or_else(|| "handoff".to_string());
    let candidate = Candidate {
        run_id: input_string_field(preserved, "source_run_id")
            .unwrap_or_else(|| "an earlier claim".to_string()),
        branch,
        head_sha,
        durable_ref: input_string_field(preserved, "durable_ref")
            .filter(|reference| valid_candidate_ref(reference)),
        needs_review_repair: failed_step_id == REVIEW_VERDICT_STEP,
        failed_step_id,
    };
    let outcome = resume(host, task_id, &candidate, workspace_path, base_sha, true)?;
    tracing::info!(
        task_id,
        outcome = outcome_name(&outcome),
        source_run_id = %candidate.run_id,
        "claimed candidate resume"
    );
    Ok(output(&outcome, Some(&candidate), base_sha))
}

/// The candidate recorded by the failure handoff of the run the task was last
/// linked to, or why there is none to resume.
fn preserved_candidate<H: RuntimeHost + ?Sized>(
    host: &H,
    task: &Task,
    prior_run_id: Option<String>,
) -> Result<Preserved, OrbitError> {
    let Some(prior_run_id) = prior_run_id else {
        return Ok(Preserved::None(
            "no earlier run is linked to the task".to_string(),
        ));
    };
    let checkpoint = host
        .read_run_state(&prior_run_id)?
        .and_then(|state| state.failure_activity_checkpoint);
    let Some(checkpoint) = checkpoint.filter(|checkpoint| {
        checkpoint.activity_name == "pr_failure_handoff"
            && checkpoint
                .output
                .get("decision")
                .and_then(Value::as_str)
                .is_some_and(|decision| PRESERVING_DECISIONS.contains(&decision))
            && checkpoint.output.get("task_id").and_then(Value::as_str) == Some(task.id.as_str())
    }) else {
        return Ok(Preserved::None(format!(
            "run '{prior_run_id}' preserved no candidate for the task"
        )));
    };
    let evidence = &checkpoint.output;
    let (Some(branch), Some(head_sha)) = (
        input_string_field(evidence, "branch"),
        input_string_field(evidence, "head_sha"),
    ) else {
        return Ok(Preserved::None(format!(
            "run '{prior_run_id}' recorded no candidate branch and head"
        )));
    };
    let candidate = Candidate {
        run_id: prior_run_id.clone(),
        branch,
        head_sha: head_sha.clone(),
        durable_ref: None,
        failed_step_id: checkpoint.failed_step_id,
        needs_review_repair: evidence["decision"] == "blocked_review_gate",
    };

    // The operator escape hatch: a discard recorded since that run began.
    let source_started = host.get_job_run(&prior_run_id)?.map(|run| run.created_at);
    let discarded = host.get_task_history(&task.id)?.iter().any(|entry| {
        entry.event == CANDIDATE_DISCARDED_EVENT
            && source_started.is_none_or(|started| entry.at >= started)
    });
    if discarded {
        let reason =
            format!("an operator discarded candidate {head_sha} from run '{prior_run_id}'");
        return Ok(Preserved::Refused(candidate, reason));
    }
    let refusal = match input_string_field(evidence, "task_spec_digest") {
        None => Some(format!(
            "candidate {head_sha} from run '{prior_run_id}' predates spec provenance"
        )),
        Some(digest) if digest != task.spec_digest() => Some(format!(
            "the task's description, acceptance criteria or selectors changed since run \
             '{prior_run_id}' produced candidate {head_sha}"
        )),
        Some(_) => None,
    };
    Ok(match refusal {
        Some(reason) => Preserved::Refused(candidate, reason),
        None => Preserved::Usable(candidate),
    })
}

fn resume<H: RuntimeHost + ?Sized>(
    host: &H,
    task_id: &str,
    candidate: &Candidate,
    workspace_path: &Path,
    base_sha: &str,
    claimed: bool,
) -> Result<Outcome, OrbitError> {
    if !candidate_available(workspace_path, candidate)? {
        let source = match &candidate.durable_ref {
            Some(reference) => format!("durable ref '{reference}'"),
            None => format!("branch '{}'", candidate.branch),
        };
        return Ok(Outcome::Fresh(format!(
            "candidate {} ({source}) is not available in this repository",
            candidate.head_sha
        )));
    }
    let head = git_output(workspace_path, &["rev-parse", "HEAD"])?;
    let status = git_output(
        workspace_path,
        &["status", "--porcelain=v1", "--untracked-files=all"],
    )?;
    if head != base_sha || !status.is_empty() {
        return Err(OrbitError::Execution(format!(
            "candidate_resume: checkout '{}' is not a clean checkout of base {base_sha} (HEAD \
             {head}); refusing to apply candidate {} over it",
            workspace_path.display(),
            candidate.head_sha
        )));
    }

    // The squash merge is a three-way merge of the candidate's changes since
    // its own base onto this base; HEAD does not move.
    let merge = git_run(
        workspace_path,
        &["merge", "--squash", "--no-commit", &candidate.head_sha],
    )?;
    if merge.timed_out {
        return Err(OrbitError::Execution(format!(
            "candidate_resume: squash merge of {} timed out after {}ms",
            candidate.head_sha, merge.timeout_ms
        )));
    }
    let conflicting_paths =
        git_output(workspace_path, &["diff", "--name-only", "--diff-filter=U"])?
            .lines()
            .map(str::to_string)
            .collect::<Vec<_>>();
    let applied = !git_command_success(workspace_path, &["diff", "--cached", "--quiet"])?;
    // Leave the result as ordinary uncommitted edits — conflict markers
    // included — and drop the merge state, so the implementer and the commit
    // step see a plain dirty checkout.
    git_success(workspace_path, &["reset", "--quiet"])?;
    if !merge.success && conflicting_paths.is_empty() {
        // Refused before touching the checkout; put back exactly what was
        // verified clean above.
        git_success(workspace_path, &["reset", "--quiet", "--hard", base_sha])?;
        return Ok(Outcome::Fresh(format!(
            "candidate {} could not be merged onto base {base_sha}: {}",
            candidate.head_sha,
            merge.stderr.trim()
        )));
    }
    if !conflicting_paths.is_empty() {
        return Ok(Outcome::Repair(json!({
            "trigger": "conflict",
            "conflicting_paths": conflicting_paths,
            "output": tail(&format!("{}\n{}", merge.stdout.trim(), merge.stderr.trim())),
        })));
    }
    if !applied {
        return Ok(Outcome::Fresh(format!(
            "candidate {}'s changes are already on base {base_sha}",
            candidate.head_sha
        )));
    }
    // A clean apply of work that never reached `commit` is not an
    // implementation. Owner validation, including an empty command list,
    // must not promote it to `resumed_validated` and skip the implementer.
    // A claimed candidate is always committed: its owner keeps none earlier.
    if !claimed && !implementation_completed(&candidate.failed_step_id) {
        return Ok(Outcome::Repair(json!({
            "trigger": "implementation",
            "failed_step_id": candidate.failed_step_id,
            "output": format!(
                "Run '{}' failed at step '{}' before `commit`, so this candidate is an \
                 unfinished implementation. Finish the task from the applied changes; a clean \
                 apply or a passing check does not make it complete.",
                candidate.run_id, candidate.failed_step_id
            ),
        })));
    }
    if candidate.failed_step_id == REVIEW_VERDICT_STEP && candidate.needs_review_repair {
        return Ok(Outcome::Repair(json!({
            "trigger": "review",
            "failed_step_id": candidate.failed_step_id,
            "output": format!(
                "The before-PR review refused this candidate in run '{}'. Its verdict and \
                 findings are in the review settlement comment on task {task_id}.",
                candidate.run_id
            ),
        })));
    }
    // A claimed implementer always runs; the leaf's own validation judges
    // what it leaves.
    if claimed {
        return Ok(Outcome::Repair(json!({
            "trigger": "continuation",
            "failed_step_id": candidate.failed_step_id,
            "output": format!(
                "Run '{}' committed this candidate and stopped at step '{}' without delivering \
                 it. It is applied onto the current base: check it against the task, finish \
                 what is missing, and keep what is already done.",
                candidate.run_id, candidate.failed_step_id
            ),
        })));
    }
    for command in host.required_validation_commands() {
        let run = run_validation_command(host, workspace_path, &command)?;
        if run.passed {
            continue;
        }
        if run.missing_tool.is_some() {
            return Ok(Outcome::Unjudged(format!(
                "required validation '{}' could not run: a tool is missing from the validation \
                 environment",
                run.command
            )));
        }
        // [ORB-14258] A base that fails the same way leaves nothing for the
        // implementer to repair; the delivery's own validation holds the
        // task again, from the shared base result.
        if compare_with_base(host, workspace_path, base_sha, &command).reproduces(&run) {
            return Ok(Outcome::Unjudged(format!(
                "required validation '{}' fails on base {base_sha} exactly as on the candidate",
                run.command
            )));
        }
        return Ok(Outcome::Repair(json!({
            "trigger": "validation",
            "command": run.command,
            "exit_code": run.exit_code,
            "timed_out": run.timed_out,
            "output": tail(&run.output),
        })));
    }
    Ok(Outcome::Validated)
}

/// Whether `failed_step_id` is `commit` or a later delivery step.
fn implementation_completed(failed_step_id: &str) -> bool {
    COMPLETED_IMPLEMENTATION_STEPS.contains(&failed_step_id)
}

/// Whether the candidate commit is in the object store, fetching it from
/// `origin` once when it is not: from the durable ref a claimed leaf carried
/// it to on another host [ORB-14338], else its branch (worktree GC may have
/// pruned the local branch after the handoff pushed it).
fn candidate_available(workspace_path: &Path, candidate: &Candidate) -> Result<bool, OrbitError> {
    let object = format!("{}^{{commit}}", candidate.head_sha);
    if git_command_success(workspace_path, &["cat-file", "-e", &object])? {
        return Ok(true);
    }
    let refspec = candidate
        .durable_ref
        .clone()
        .unwrap_or_else(|| format!("refs/heads/{}", candidate.branch));
    let _ = git_run(workspace_path, &["fetch", "--no-tags", "origin", &refspec])?;
    git_command_success(workspace_path, &["cat-file", "-e", &object])
}

/// Write the outcome to the task's history.
fn record<H: RuntimeHost + ?Sized>(
    host: &H,
    task: &Task,
    run_id: &str,
    candidate: &Candidate,
    outcome: &Outcome,
) -> Result<(), OrbitError> {
    let detail = match outcome {
        Outcome::Fresh(reason) | Outcome::Unjudged(reason) => format!("; {reason}"),
        Outcome::Repair(repair) => format!(
            "; repair trigger: {}",
            repair["trigger"].as_str().unwrap_or("unknown")
        ),
        Outcome::Validated => String::new(),
    };
    let note = format!(
        "{}: run={run_id}, source_run={}, source_branch={}, source_sha={}{detail}",
        outcome_name(outcome),
        candidate.run_id,
        candidate.branch,
        candidate.head_sha,
    );
    host.apply_task_automation_update(
        &task.id,
        TaskAutomationUpdate {
            status_event: Some(CANDIDATE_RESUME_EVENT.to_string()),
            status_note: Some(note),
            ..TaskAutomationUpdate::default()
        },
    )?;
    tracing::info!(task_id = %task.id, run_id, outcome = outcome_name(outcome), "candidate resume");
    Ok(())
}

fn outcome_name(outcome: &Outcome) -> &'static str {
    match outcome {
        Outcome::Fresh(_) => "fresh",
        Outcome::Validated => "resumed_validated",
        Outcome::Unjudged(_) => "resumed_unjudged",
        Outcome::Repair(_) => "resumed_repaired",
    }
}

fn output(outcome: &Outcome, candidate: Option<&Candidate>, base_sha: &str) -> Value {
    let (reason, repair) = match outcome {
        Outcome::Fresh(reason) | Outcome::Unjudged(reason) => (Some(reason.as_str()), Value::Null),
        Outcome::Repair(repair) => (None, repair.clone()),
        Outcome::Validated => (None, Value::Null),
    };
    json!({
        "phase": "candidate_resume",
        "outcome": outcome_name(outcome),
        "implement": matches!(outcome, Outcome::Fresh(_) | Outcome::Repair(_)),
        "reason": reason,
        "repair": repair,
        "source_run_id": candidate.map(|candidate| candidate.run_id.as_str()),
        "source_branch": candidate.map(|candidate| candidate.branch.as_str()),
        "source_sha": candidate.map(|candidate| candidate.head_sha.as_str()),
        "base_sha": base_sha,
    })
}

/// The last [`MAX_REPAIR_OUTPUT_BYTES`] of `text`, marked when cut.
fn tail(text: &str) -> String {
    let text = text.trim();
    if text.len() <= MAX_REPAIR_OUTPUT_BYTES {
        return text.to_string();
    }
    let mut start = text.len() - MAX_REPAIR_OUTPUT_BYTES;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    format!(
        "[first {start} of {} bytes cut]\n{}",
        text.len(),
        &text[start..]
    )
}
