use std::path::Path;

use orbit_common::OrbitError;
use orbit_store::contracts::{CandidateFreshReason, KeptClaimCandidate};
use orbit_types::task::{CANDIDATE_DISCARDED_EVENT, Task, TaskHistoryEntry, TaskStatus};
use orbit_types::workflow::{
    REVIEW_EVIDENCE_HOLD_ARTIFACT, REVIEW_EVIDENCE_RECEIVED_EVENT, ReviewEvidenceHold,
};
use serde_json::{Value, json};

use crate::context::RuntimeHost;
use crate::executor::automation::input::{
    canonicalize_existing_dir, input_string_field, required_input_string, required_job_run_id,
};

use super::super::operations::valid_candidate_ref;
use super::adopt::{Adoption, adopt_published_branch};
use super::apply::{Applied, apply, outcome_name, output, record, resume, tail};
use super::{
    Candidate, ClaimSource, ForeignRun, Fresh, Outcome, PRESERVING_DECISIONS, Preserved,
    REVIEW_VERDICT_STEP,
};

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
            &Outcome::Fresh(Fresh::new(
                "bundle",
                "a bundle run implements every task fresh",
            )),
            None,
            &base_sha,
        ));
    };
    if input.get("claimed").and_then(Value::as_bool) == Some(true) {
        if let Some(repair) = input
            .get("claim_repair")
            .filter(|repair| repair.is_object())
        {
            return claim_repair_resume(repair, &workspace_path, &base_sha);
        }
        return claimed_resume(host, input, task_id, &workspace_path, &base_sha);
    }
    let task = host.get_task(task_id)?;
    let foreign = input.get("prior_foreign_run").and_then(|run| {
        Some(ForeignRun {
            run_id: input_string_field(run, "run_id")?,
            machine_id: input_string_field(run, "machine_id")?,
        })
    });
    let preserved = match &foreign {
        Some(foreign) => foreign_candidate(host, &task, foreign)?,
        None => {
            let prior_run_id = input_string_field(input, "prior_job_run_id");
            preserved_candidate(host, &task, prior_run_id)?
        }
    };
    // A claim candidate whose leaf's evidence hold was the task's latest
    // decision until its evidence arrived is the held candidate itself.
    let preserved = match preserved {
        Preserved::Usable(mut candidate)
            if candidate.claim.is_some()
                && evidence_received(&host.get_task_history(&task.id)?, &candidate.run_id) =>
        {
            candidate.held = true;
            candidate.needs_review_repair = false;
            Preserved::Usable(candidate)
        }
        preserved => preserved,
    };
    let (candidate, outcome) = match preserved {
        Preserved::None(reason) => return Ok(output(&Outcome::Fresh(reason), None, &base_sha)),
        Preserved::Refused(candidate, reason) => (candidate, Outcome::Fresh(reason)),
        Preserved::Usable(candidate) => {
            let from_claim = candidate.claim.is_some();
            let outcome = resume(
                host,
                &task.id,
                &candidate,
                &workspace_path,
                &base_sha,
                from_claim,
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
    let Some(candidate) = input
        .get("candidate")
        .and_then(|preserved| claim_candidate(preserved, None))
    else {
        return Ok(output(
            &Outcome::Fresh(Fresh::new(
                "no_candidate",
                "the claim carries no preserved candidate",
            )),
            None,
            base_sha,
        ));
    };
    let outcome = resume(host, task_id, &candidate, workspace_path, base_sha, true)?;
    tracing::info!(target: "orbit_engine::executor::automation::vcs::candidate_resume",
        task_id,
        outcome = outcome_name(&outcome),
        source_run_id = %candidate.run_id,
        "claimed candidate resume"
    );
    let mut resumed = output(&outcome, Some(&candidate), base_sha);
    let Some(pull_request) = &candidate.pull_request else {
        return Ok(resumed);
    };
    // [ORB-15308] The earlier claim opened a pull request for this
    // candidate. Continue on its branch so this leaf's push and `pr_open`
    // land on that pull request; otherwise `pr_open` closes it once this
    // leaf's own is open.
    resumed["prior_pull_request"] = json!(pull_request);
    let adoption = match outcome {
        Outcome::Fresh(_) => Adoption::Refused("the candidate was not resumed".to_string()),
        _ if !candidate.published => Adoption::Refused(format!(
            "branch '{}' was not pushed at {}",
            candidate.branch, candidate.head_sha
        )),
        _ => adopt_published_branch(workspace_path, task_id, &candidate)?,
    };
    match adoption {
        Adoption::Adopted => {
            resumed["reused_branch"] = json!(candidate.branch);
            resumed["reused_head_sha"] = json!(candidate.head_sha);
        }
        Adoption::Refused(reason) => {
            tracing::warn!(
                task_id,
                pull_request = %pull_request,
                branch = %candidate.branch,
                reason = %reason,
                "claimed leaf cannot continue its candidate's pull request; it will supersede it"
            );
            resumed["branch_reuse_refused"] = json!(reason);
        }
    }
    Ok(resumed)
}

/// The candidate a claim's leaf committed, from its kept reference (a
/// `ClaimCandidateRef`); `None` without a branch and head.
fn claim_candidate(preserved: &Value, claim: Option<ClaimSource>) -> Option<Candidate> {
    let branch = input_string_field(preserved, "branch")?;
    let head_sha = input_string_field(preserved, "head_sha")?;
    // A leaf that stopped after its last delivery step names none; its
    // candidate is complete.
    let failed_step_id =
        input_string_field(preserved, "failed_step_id").unwrap_or_else(|| "handoff".to_string());
    Some(Candidate {
        run_id: input_string_field(preserved, "source_run_id")
            .unwrap_or_else(|| "an earlier claim".to_string()),
        branch,
        head_sha,
        durable_ref: input_string_field(preserved, "durable_ref")
            .filter(|reference| valid_candidate_ref(reference)),
        needs_review_repair: failed_step_id == REVIEW_VERDICT_STEP,
        failed_step_id,
        held: false,
        claim,
        published: preserved.get("published").and_then(Value::as_bool) == Some(true),
        pull_request: input_string_field(preserved, "pull_request"),
    })
}

/// The candidate recorded by the failure handoff of the run the task was last
/// linked to, or else the one its evidence hold kept, or why there is none to
/// resume.
fn preserved_candidate<H: RuntimeHost + ?Sized>(
    host: &H,
    task: &Task,
    prior_run_id: Option<String>,
) -> Result<Preserved, OrbitError> {
    let Some(prior_run_id) = prior_run_id else {
        return Ok(Preserved::None(Fresh::new(
            "no_prior_run",
            "no earlier run is linked to the task",
        )));
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
        return match held_candidate(host, task, &prior_run_id)? {
            Preserved::None(reason) => Ok(local_claim_candidate(host, task, &prior_run_id)?
                .unwrap_or(Preserved::None(reason))),
            preserved => Ok(preserved),
        };
    };
    let evidence = &checkpoint.output;
    let (Some(branch), Some(head_sha)) = (
        input_string_field(evidence, "branch"),
        input_string_field(evidence, "head_sha"),
    ) else {
        return Ok(Preserved::None(Fresh::new(
            "no_candidate",
            format!("run '{prior_run_id}' recorded no candidate branch and head"),
        )));
    };
    let candidate = Candidate {
        run_id: prior_run_id.clone(),
        branch,
        head_sha: head_sha.clone(),
        // [ORB-14905] A hold carried its candidate to `origin` too, which
        // still serves it once this host's branch or worktree is gone.
        durable_ref: input_string_field(evidence, "durable_ref")
            .filter(|reference| valid_candidate_ref(reference)),
        failed_step_id: checkpoint.failed_step_id,
        needs_review_repair: evidence["decision"] == "blocked_review_gate",
        held: false,
        claim: None,
        published: false,
        pull_request: None,
    };
    let recorded = input_string_field(evidence, "task_spec_digest");
    refuse_stale(host, task, candidate, Some(recorded.as_deref()))
}

/// [ORB-14450] The candidate the task's evidence hold kept from run
/// `prior_run_id`, when the task's latest decision is that hold's evidence
/// receipt. A held run ends before the failure handoff, so the hold artifact
/// is what names the exact commit the evidence was checked against.
fn held_candidate<H: RuntimeHost + ?Sized>(
    host: &H,
    task: &Task,
    prior_run_id: &str,
) -> Result<Preserved, OrbitError> {
    let none = || {
        Ok(Preserved::None(Fresh::new(
            "no_candidate",
            format!("run '{prior_run_id}' preserved no candidate for the task"),
        )))
    };
    let hold = host
        .get_task_artifacts(&task.id)?
        .into_iter()
        .find(|artifact| {
            artifact.path == REVIEW_EVIDENCE_HOLD_ARTIFACT
                && artifact.created_by.as_deref() == Some("system")
        })
        .and_then(|artifact| serde_json::from_slice::<ReviewEvidenceHold>(&artifact.content).ok())
        .filter(|hold| {
            hold.schema_version == 1
                && hold.run_id == prior_run_id
                && !hold.candidate.commit.trim().is_empty()
        });
    let Some(hold) = hold else {
        return none();
    };
    if !evidence_received(&host.get_task_history(&task.id)?, prior_run_id) {
        return none();
    }
    let branch = host
        .read_run_state(prior_run_id)?
        .and_then(|state| {
            state
                .pipeline
                .get("worktree")
                .and_then(|worktree| input_string_field(worktree, "head_ref"))
        })
        .unwrap_or_default();
    let candidate = Candidate {
        run_id: prior_run_id.to_string(),
        branch,
        head_sha: hold.candidate.commit,
        durable_ref: None,
        failed_step_id: REVIEW_VERDICT_STEP.to_string(),
        needs_review_repair: false,
        held: true,
        claim: None,
        published: false,
        pull_request: None,
    };
    // A hold from before spec provenance was released by a receipt that
    // checked the task's whole meaning; the fresh review reads the task as
    // it is now.
    let recorded = hold.task_spec_digest.as_deref().map(Some);
    refuse_stale(host, task, candidate, recorded)
}

/// [ORB-14603] What an owner-local run resumes when the task's prior run
/// executed on another machine: the candidate the owner kept from the task's
/// last claim, never anything this machine's run store holds under that
/// run's id.
fn foreign_candidate<H: RuntimeHost + ?Sized>(
    host: &H,
    task: &Task,
    foreign: &ForeignRun,
) -> Result<Preserved, OrbitError> {
    Ok(match host.kept_claim_candidate(&task.id)? {
        Some(kept) => kept_candidate(kept)?,
        None => Preserved::None(Fresh::new(
            "no_candidate",
            format!(
                "run '{}' on machine '{}' preserved no candidate the owner kept for the task",
                foreign.run_id, foreign.machine_id
            ),
        )),
    })
}

/// [ORB-14603] The candidate the owner kept from the claim whose leaf was
/// this machine's run `prior_run_id`, when that leaf left no failure handoff.
fn local_claim_candidate<H: RuntimeHost + ?Sized>(
    host: &H,
    task: &Task,
    prior_run_id: &str,
) -> Result<Option<Preserved>, OrbitError> {
    let local = host.local_machine_id();
    host.kept_claim_candidate(&task.id)?
        .filter(|kept| {
            local.as_deref() == Some(kept.machine_id.as_str())
                && kept.candidate.source_run_id.as_deref() == Some(prior_run_id)
        })
        .map(kept_candidate)
        .transpose()
}

/// A candidate the owner kept from a claim, or why it refused it.
fn kept_candidate(kept: KeptClaimCandidate) -> Result<Preserved, OrbitError> {
    let KeptClaimCandidate {
        claim_id,
        machine_id,
        candidate,
        fresh,
    } = kept;
    let source = format!("claim '{claim_id}' on machine '{machine_id}'");
    let reference = serde_json::to_value(&candidate).map_err(|error| {
        OrbitError::Execution(format!(
            "candidate_resume: candidate kept from {source} is unreadable: {error}"
        ))
    })?;
    let Some(candidate) = claim_candidate(
        &reference,
        Some(ClaimSource {
            claim_id,
            machine_id,
        }),
    ) else {
        return Ok(Preserved::None(Fresh::new(
            "no_candidate",
            format!("{source} kept no candidate branch and head"),
        )));
    };
    let Some((reason, detail)) = fresh else {
        return Ok(Preserved::Usable(candidate));
    };
    let code = match reason {
        CandidateFreshReason::NotDurable => "not_durable",
        CandidateFreshReason::SpecChanged => "spec_changed",
        CandidateFreshReason::Discarded => "candidate_discarded",
    };
    let reason = Fresh::new(
        code,
        format!("candidate {} from {source}: {detail}", candidate.head_sha),
    );
    Ok(Preserved::Refused(candidate, reason))
}

/// Whether the latest status decision in `history` is the evidence receipt
/// for held run `run_id`. Admission into this run is not a decision.
fn evidence_received(history: &[TaskHistoryEntry], run_id: &str) -> bool {
    history
        .iter()
        .rev()
        .find(|entry| {
            entry.event == REVIEW_EVIDENCE_RECEIVED_EVENT
                || entry
                    .to_status
                    .is_some_and(|status| status != TaskStatus::InProgress)
        })
        .is_some_and(|entry| {
            entry.event == REVIEW_EVIDENCE_RECEIVED_EVENT
                && entry
                    .note
                    .as_deref()
                    .is_some_and(|note| note.starts_with(&format!("run={run_id};")))
        })
}

/// Refuse `candidate` when an operator discarded it since its run began, or
/// the task's description or acceptance criteria changed since
/// `recorded_spec` was taken. `Some(None)` is a record that should carry a
/// spec digest and does not; `None` is one that never did.
fn refuse_stale<H: RuntimeHost + ?Sized>(
    host: &H,
    task: &Task,
    candidate: Candidate,
    recorded_spec: Option<Option<&str>>,
) -> Result<Preserved, OrbitError> {
    let (run_id, head_sha) = (&candidate.run_id, &candidate.head_sha);
    // The operator escape hatch: a discard recorded since that run began.
    let source_started = host.get_job_run(run_id)?.map(|run| run.created_at);
    let discarded = host.get_task_history(&task.id)?.iter().any(|entry| {
        entry.event == CANDIDATE_DISCARDED_EVENT
            && source_started.is_none_or(|started| entry.at >= started)
    });
    if discarded {
        let reason = Fresh::new(
            "candidate_discarded",
            format!("an operator discarded candidate {head_sha} from run '{run_id}'"),
        );
        return Ok(Preserved::Refused(candidate, reason));
    }
    if recorded_spec == Some(None) {
        let reason = Fresh::new(
            "spec_unrecorded",
            format!("candidate {head_sha} from run '{run_id}' predates spec provenance"),
        );
        return Ok(Preserved::Refused(candidate, reason));
    }
    if recorded_spec
        .flatten()
        .is_some_and(|digest| !task.spec_digest_matches(digest))
    {
        let reason = Fresh::new(
            "spec_changed",
            format!(
                "the task's description or acceptance criteria changed since run '{run_id}' \
                 produced candidate {head_sha}"
            ),
        );
        return Ok(Preserved::Refused(candidate, reason));
    }
    Ok(Preserved::Usable(candidate))
}

/// [ORB-14261] Resume the candidate a repair claim carries: the one whose
/// owner landing stopped on its base. Unlike a kept candidate it must not be
/// dropped, so a candidate that cannot be restored fails the leaf.
fn claim_repair_resume(
    repair: &Value,
    workspace_path: &Path,
    base_sha: &str,
) -> Result<Value, OrbitError> {
    let (Some(branch), Some(head_sha)) = (
        input_string_field(repair, "branch"),
        input_string_field(repair, "head_sha"),
    ) else {
        return Err(OrbitError::InvalidInput(
            "candidate_resume: claim_repair requires the candidate's branch and head_sha"
                .to_string(),
        ));
    };
    let stopped = input_string_field(repair, "stop_evidence")
        .unwrap_or_else(|| "the owner's landing stopped on its base".to_string());
    let candidate = Candidate {
        run_id: input_string_field(repair, "repairs_claim_id").unwrap_or_default(),
        branch,
        head_sha,
        durable_ref: None,
        failed_step_id: "landing".to_string(),
        needs_review_repair: false,
        held: false,
        claim: None,
        published: false,
        pull_request: None,
    };
    let outcome = match apply(&candidate, workspace_path, base_sha)? {
        Applied::Refused(reason) => {
            return Err(OrbitError::Execution(format!(
                "candidate_resume: repair candidate {} could not be restored: {reason}",
                candidate.head_sha
            )));
        }
        Applied::Conflict { paths, output } => Outcome::Repair(json!({
            "trigger": "conflict",
            "conflicting_paths": paths,
            "output": tail(&format!(
                "The owner's landing of this candidate stopped: {stopped}\n\n{output}"
            )),
        })),
        Applied::Clean => Outcome::Repair(json!({
            "trigger": "landing",
            "output": tail(&format!(
                "The owner's landing of this candidate stopped: {stopped}\n\nIt applied cleanly \
                 onto the current base {base_sha}; confirm it still meets the acceptance \
                 criteria there."
            )),
        })),
        Applied::AlreadyPresent => Outcome::Repair(json!({
            "trigger": "landing",
            "output": tail(&format!(
                "The owner's landing of this candidate stopped: {stopped}\n\nIts changes are \
                 already present on the current base {base_sha}; confirm they still meet the \
                 acceptance criteria there."
            )),
        })),
    };
    tracing::info!(target: "orbit_engine::executor::automation::vcs::candidate_resume",
        head_sha = %candidate.head_sha,
        branch = %candidate.branch,
        outcome = outcome_name(&outcome),
        "candidate resume for a repair claim"
    );
    let mut resumed = output(&outcome, None, base_sha);
    resumed["source_branch"] = json!(candidate.branch);
    resumed["source_sha"] = json!(candidate.head_sha);
    Ok(resumed)
}
