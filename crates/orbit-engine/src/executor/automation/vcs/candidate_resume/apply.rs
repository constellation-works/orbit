use std::path::Path;

use orbit_common::OrbitError;
use orbit_types::task::{CANDIDATE_RESUME_EVENT, Task};
use serde_json::{Value, json};

use crate::context::{RuntimeHost, TaskAutomationUpdate};

use super::super::baseline::{compare_with_base, run_validation_command};
use super::super::git::{git_command_success, git_output, git_run, git_success};
use super::{
    COMPLETED_IMPLEMENTATION_STEPS, Candidate, Fresh, MAX_REPAIR_OUTPUT_BYTES, Outcome,
    REVIEW_VERDICT_STEP,
};

/// What squash-merging a candidate onto a clean base checkout left behind.
pub(super) enum Applied {
    /// Nothing usable was applied, and why; the checkout is the clean base.
    Refused(String),
    /// The candidate's changes are already present on the clean base.
    AlreadyPresent,
    /// Uncommitted changes with conflict markers in `paths`.
    Conflict { paths: Vec<String>, output: String },
    /// Uncommitted changes that applied without conflict.
    Clean,
}

/// Squash-merge `candidate` onto the clean checkout of `base_sha`, leaving the
/// result as plain uncommitted edits — conflict markers included.
pub(super) fn apply(
    candidate: &Candidate,
    workspace_path: &Path,
    base_sha: &str,
) -> Result<Applied, OrbitError> {
    if !candidate_available(workspace_path, candidate)? {
        let source = match &candidate.durable_ref {
            Some(reference) => format!("durable ref '{reference}'"),
            None => format!("branch '{}'", candidate.branch),
        };
        return Ok(Applied::Refused(format!(
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
        return Ok(Applied::Refused(format!(
            "candidate {} could not be merged onto base {base_sha}: {}",
            candidate.head_sha,
            merge.stderr.trim()
        )));
    }
    if !conflicting_paths.is_empty() {
        return Ok(Applied::Conflict {
            paths: conflicting_paths,
            output: format!("{}\n{}", merge.stdout.trim(), merge.stderr.trim()),
        });
    }
    if !applied {
        return Ok(Applied::AlreadyPresent);
    }
    Ok(Applied::Clean)
}

/// Apply `candidate` and judge it. `claimed` is a candidate a claim's leaf
/// committed, resumed by a claimed leaf or by the owner's own run
/// [ORB-14603].
pub(super) fn resume<H: RuntimeHost + ?Sized>(
    host: &H,
    task_id: &str,
    candidate: &Candidate,
    workspace_path: &Path,
    base_sha: &str,
    claimed: bool,
) -> Result<Outcome, OrbitError> {
    match apply(candidate, workspace_path, base_sha)? {
        Applied::Refused(reason) => {
            return Ok(Outcome::Fresh(Fresh::new("candidate_missing", reason)));
        }
        Applied::AlreadyPresent => {
            return Ok(Outcome::Fresh(Fresh::new(
                "already_on_base",
                format!(
                    "candidate {}'s changes are already on base {base_sha}",
                    candidate.head_sha
                ),
            )));
        }
        Applied::Conflict { paths, output } => {
            let output = if candidate.held {
                format!(
                    "{output}\n\nThis candidate was held on named external evidence; once the \
                     conflict is resolved its patch differs, so the review gate requests that \
                     evidence again."
                )
            } else {
                output
            };
            return Ok(Outcome::Repair(json!({
                "trigger": "conflict",
                "conflicting_paths": paths,
                "output": tail(&output),
            })));
        }
        Applied::Clean => {}
    }
    // The held candidate is the one the review held and the evidence was
    // checked on: the pipeline's own validation and fresh review judge it.
    if candidate.held {
        return Ok(Outcome::Held);
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
                "The before-PR review refused this candidate ({}). Its verdict and findings are \
                 in the review settlement comment on task {task_id}.",
                candidate.source()
            ),
        })));
    }
    // A claimed implementer always runs; the run's own validation judges
    // what it leaves.
    if claimed {
        return Ok(Outcome::Repair(json!({
            "trigger": "continuation",
            "failed_step_id": candidate.failed_step_id,
            "output": format!(
                "{} committed this candidate and stopped at step '{}' without delivering it. It \
                 is applied onto the current base: check it against the task, finish what is \
                 missing, and keep what is already done.",
                candidate.source(),
                candidate.failed_step_id
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
    let refspec = match (&candidate.durable_ref, candidate.branch.as_str()) {
        (Some(reference), _) => reference.clone(),
        // A held run's checkout may be gone with no branch on record.
        (None, "") => return Ok(false),
        (None, branch) => format!("refs/heads/{branch}"),
    };
    let _ = git_run(workspace_path, &["fetch", "--no-tags", "origin", &refspec])?;
    git_command_success(workspace_path, &["cat-file", "-e", &object])
}

/// Write the outcome to the task's history.
pub(super) fn record<H: RuntimeHost + ?Sized>(
    host: &H,
    task: &Task,
    run_id: &str,
    candidate: &Candidate,
    outcome: &Outcome,
) -> Result<(), OrbitError> {
    let detail = match outcome {
        Outcome::Fresh(reason) => format!("; reason_code={}; {}", reason.code, reason.detail),
        Outcome::Unjudged(reason) => format!("; {reason}"),
        Outcome::Repair(repair) => format!(
            "; repair trigger: {}",
            repair["trigger"].as_str().unwrap_or("unknown")
        ),
        Outcome::Validated | Outcome::Held => String::new(),
    };
    let claim = candidate.claim.as_ref().map_or_else(String::new, |claim| {
        format!("claim={}, machine={}, ", claim.claim_id, claim.machine_id)
    });
    let note = format!(
        "{}: run={run_id}, {claim}source_run={}, source_branch={}, source_sha={}{detail}",
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
    tracing::info!(target: "orbit_engine::executor::automation::vcs::candidate_resume", task_id = %task.id, run_id, outcome = outcome_name(outcome), "candidate resume");
    Ok(())
}

pub(super) fn outcome_name(outcome: &Outcome) -> &'static str {
    match outcome {
        Outcome::Fresh(_) => "fresh",
        Outcome::Validated => "resumed_validated",
        Outcome::Held => "resumed_held",
        Outcome::Unjudged(_) => "resumed_unjudged",
        Outcome::Repair(_) => "resumed_repaired",
    }
}

pub(super) fn output(outcome: &Outcome, candidate: Option<&Candidate>, base_sha: &str) -> Value {
    let (reason, reason_code, repair) = match outcome {
        Outcome::Fresh(reason) => (Some(reason.detail.as_str()), Some(reason.code), Value::Null),
        Outcome::Unjudged(reason) => (Some(reason.as_str()), None, Value::Null),
        Outcome::Repair(repair) => (None, None, repair.clone()),
        Outcome::Validated | Outcome::Held => (None, None, Value::Null),
    };
    json!({
        "phase": "candidate_resume",
        "outcome": outcome_name(outcome),
        "implement": matches!(outcome, Outcome::Fresh(_) | Outcome::Repair(_)),
        "reason": reason,
        "reason_code": reason_code,
        "repair": repair,
        "source_run_id": candidate.map(|candidate| candidate.run_id.as_str()),
        "source_machine_id": candidate
            .and_then(|candidate| candidate.claim.as_ref())
            .map(|claim| claim.machine_id.as_str()),
        "source_branch": candidate.map(|candidate| candidate.branch.as_str()),
        "source_sha": candidate.map(|candidate| candidate.head_sha.as_str()),
        "base_sha": base_sha,
    })
}

/// The last [`MAX_REPAIR_OUTPUT_BYTES`] of `text`, marked when cut.
pub(super) fn tail(text: &str) -> String {
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
