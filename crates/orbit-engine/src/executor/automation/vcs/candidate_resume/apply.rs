use std::path::Path;

use orbit_common::OrbitError;
use orbit_store::contracts::InterruptedReview;
use orbit_types::task::{CANDIDATE_RESUME_EVENT, Task};
use serde_json::{Value, json};

use crate::context::{RuntimeHost, TaskAutomationUpdate};

use super::super::git::{
    git_command_success, git_output, git_run, git_success, timeout_recovery_error,
};
use super::super::interrupted_review::locate_review_repair;
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

/// Squash-merge `candidate` — its commit `target`, which is its head or a
/// kept review repair on it — onto the clean checkout of `base_sha`, leaving
/// the result as plain uncommitted edits — conflict markers included.
pub(super) fn apply(
    candidate: &Candidate,
    target: &str,
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
        &["merge", "--squash", "--no-commit", target],
    )?;
    if merge.timed_out {
        return Err(timeout_recovery_error(
            merge.timeout_ms,
            format!(
                "candidate_resume: squash merge of {target} timed out after {}ms",
                merge.timeout_ms
            ),
        ));
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
            "candidate {target} could not be merged onto base {base_sha}: {}",
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

/// Apply `candidate` and decide whether it needs the implementer. `claimed`
/// is a candidate a claim's leaf committed, resumed by a claimed leaf or by
/// the owner's own run
/// [ORB-14603].
pub(super) fn resume(
    task_id: &str,
    candidate: &Candidate,
    workspace_path: &Path,
    base_sha: &str,
    claimed: bool,
) -> Result<Outcome, OrbitError> {
    // [ORB-15314] A review stopped mid-way kept its reviewer's repair as a
    // commit on the head; resume that, or the head alone when it is gone.
    let interrupted = match &candidate.interrupted_review {
        Some(review) => Some((
            review,
            match &review.repair_ref {
                Some(repair_ref) => locate_review_repair(
                    workspace_path,
                    &candidate.head_sha,
                    &candidate.run_id,
                    repair_ref,
                    review.repair_commit.as_deref(),
                )?,
                None => Err("the reviewer left no uncommitted repair to keep".to_string()),
            },
        )),
        None => None,
    };
    let target = match &interrupted {
        Some((_, Ok(repair))) => repair.as_str(),
        _ => candidate.head_sha.as_str(),
    };
    let reviewed = |repair: Value| match &interrupted {
        Some((review, located)) => with_interrupted_review(repair, candidate, review, located),
        None => repair,
    };
    match apply(candidate, target, workspace_path, base_sha)? {
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
            let output = match &candidate.landing_stop {
                Some(stop) => {
                    format!("The owner's landing of this candidate stopped: {stop}\n\n{output}")
                }
                None => output,
            };
            return Ok(Outcome::Repair(reviewed(json!({
                "trigger": "conflict",
                "conflicting_paths": paths,
                "output": tail(&output),
            }))));
        }
        Applied::Clean => {}
    }
    // The held candidate is the one the review held and the evidence was
    // checked on: the pipeline's own validation and fresh review judge it.
    if candidate.held {
        return Ok(Outcome::Held);
    }
    // A clean apply of work that never reached `commit` is unfinished and
    // must not skip the implementer, regardless of validation commands.
    if !implementation_completed(&candidate.failed_step_id) {
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
    // [ORB-16890] The owner's landing stopped this handed-off candidate on
    // a required check it fails: the implementer repairs what that check
    // names, whoever runs it.
    if let Some(stop) = &candidate.landing_stop {
        return Ok(Outcome::Repair(json!({
            "trigger": "landing",
            "failed_step_id": candidate.failed_step_id,
            "output": tail(&format!(
                "The owner's landing of this candidate stopped: {stop}\n\nIt applied cleanly \
                 onto the current base {base_sha}. Repair what the failing check names, keep the \
                 rest of the candidate, and rerun that check."
            )),
        })));
    }
    if interrupted.is_some() {
        return Ok(Outcome::Repair(reviewed(json!({
            "trigger": "review",
            "failed_step_id": candidate.failed_step_id,
        }))));
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
    // A completed owner-local candidate is carried without judging it here.
    // The ordinary delivery gate runs on its committed, synchronized head;
    // that gate owns candidate repair, environment failures and red-base holds.
    Ok(Outcome::Carried)
}

/// [ORB-15314] `repair` with the stopped review's record for the
/// implementer: its findings, and whether the reviewer's kept repair is part
/// of the applied changes. A clean apply's `output` says how to continue.
fn with_interrupted_review(
    mut repair: Value,
    candidate: &Candidate,
    review: &InterruptedReview,
    located: &Result<String, String>,
) -> Value {
    let (repair_commit, refused) = match located {
        Ok(commit) => (Some(commit.as_str()), None),
        Err(why) => (None, Some(why.as_str())),
    };
    repair["interrupted_review"] = json!({
        "step_id": review.step_id,
        "attempt_id": review.attempt_id,
        "verdict": review.verdict,
        "findings": review.findings,
        "repair_ref": review.repair_ref,
        "repair_commit": repair_commit,
        "repair_applied": repair_commit.is_some(),
        "repair_refused": refused,
    });
    if repair.get("output").is_none() {
        let kept = match refused {
            None => "The reviewer's uncommitted repair was kept and is applied with it; it was \
                     never validated or settled, so check it like your own work."
                .to_string(),
            Some(why) => format!("Only the candidate is applied: {why}."),
        };
        repair["output"] = json!(format!(
            "{} was stopped while its review step '{}' was running, so the review never \
             settled. The candidate is applied onto the current base. {kept} Repair the \
             candidate rather than re-implementing it: address the findings in \
             `interrupted_review.findings` whose `disposition` is `open`, confirm the ones \
             marked `repaired` against the applied changes, keep what is already done, and \
             finish what is missing.",
            candidate.source(),
            review.step_id
        ));
    }
    repair
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
        Outcome::Repair(repair) => format!(
            "; repair trigger: {}",
            repair["trigger"].as_str().unwrap_or("unknown")
        ),
        Outcome::Carried | Outcome::Held => String::new(),
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
        Outcome::Carried => "resumed_carried",
        Outcome::Held => "resumed_held",
        Outcome::Repair(_) => "resumed_repaired",
    }
}

pub(super) fn output(outcome: &Outcome, candidate: Option<&Candidate>, base_sha: &str) -> Value {
    let (reason, reason_code, repair) = match outcome {
        Outcome::Fresh(reason) => (Some(reason.detail.as_str()), Some(reason.code), Value::Null),
        Outcome::Repair(repair) => (None, None, repair.clone()),
        Outcome::Carried | Outcome::Held => (None, None, Value::Null),
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
        // [ORB-15308] Set when the candidate's claim opened a pull request,
        // with its head branch [ORB-15570].
        "prior_pull_request": null,
        "prior_pull_request_head": null,
        "reused_branch": null,
        "reused_head_sha": null,
        "branch_reuse_refused": null,
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
