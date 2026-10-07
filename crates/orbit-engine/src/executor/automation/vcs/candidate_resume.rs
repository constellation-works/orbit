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
//!   reason and its `reason_code`.
//!
//! "The spec" is the task's description and acceptance criteria. Context
//! selectors are preparation hints, so a selector edit keeps the candidate;
//! the resumed review reads the current selectors [ORB-14450].
//!
//! [ORB-14450] A run held on named external evidence ends before the failure
//! handoff, so it records no checkpoint. When the task's latest decision is
//! the evidence receipt for that held run, its candidate — the exact commit
//! the task's evidence hold names — is resumed instead: a clean apply is
//! `resumed_held`, and no implementation step or owner validation runs here,
//! so the run goes on to commit, validation and the fresh review that finds
//! the evidence. On the hold's own base the squash reproduces the held tree;
//! on a moved base the review gate counts the evidence only while the patch
//! is unchanged. A conflict hands the implementer the conflict, after which
//! the gate requests the evidence again.
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
//!
//! [ORB-14603] When the task's prior run is one another machine executed — a
//! claim's leaf, handed in as `prior_foreign_run` with that machine — its id
//! is never looked up in this machine's run store, where it may name
//! unrelated work. An owner-local run continues the candidate the owner kept
//! from the task's last claim instead, as a claimed leaf would: the owner
//! offers it under the same discard, spec and fetchability checks, a refusal
//! is `fresh` with that typed reason, and every outcome is written to the
//! task's history naming the claim and the machine that committed it. A
//! claim candidate was always committed, so its implementer runs as a
//! claimed leaf's does. A prior run on this machine that recorded no
//! failure handoff — a claim's leaf this machine executed — falls back to
//! the candidate the owner kept from that run's claim.
//!
//! A repair claim's leaf passes `claim_repair` instead [ORB-14261]: the
//! candidate an owner's landing stopped on a base conflict or stale base.
//! It is squash-merged the same way and the implementer always runs — on a
//! `conflict` to resolve, or on a `landing` repair that applied cleanly onto
//! the moved base. If that candidate cannot be restored, the leaf fails
//! closed instead of implementing fresh and silently dropping its work.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_store::contracts::{CandidateFreshReason, KeptClaimCandidate};
use orbit_types::task::{
    CANDIDATE_DISCARDED_EVENT, CANDIDATE_RESUME_EVENT, Task, TaskHistoryEntry, TaskStatus,
};
use orbit_types::workflow::{
    REVIEW_EVIDENCE_HOLD_ARTIFACT, REVIEW_EVIDENCE_RECEIVED_EVENT, ReviewEvidenceHold,
};
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
    /// [ORB-14450] The candidate an evidence hold kept, whose evidence
    /// arrived: resumed without the implementer or owner validation.
    held: bool,
    /// [ORB-14603] The claim whose settlement the owner kept it from, and
    /// the machine that claim executed on, for an owner-local run's resume.
    claim: Option<ClaimSource>,
}

impl Candidate {
    /// The run that produced it, with the machine that executed it when that
    /// was a claim's leaf: its id alone names no run on this machine.
    fn source(&self) -> String {
        match &self.claim {
            Some(claim) => format!("Run '{}' on machine '{}'", self.run_id, claim.machine_id),
            None => format!("Run '{}'", self.run_id),
        }
    }
}

/// The claim a kept candidate came from.
struct ClaimSource {
    claim_id: String,
    machine_id: String,
}

/// [ORB-14603] The task's prior run, executed on another machine.
struct ForeignRun {
    run_id: String,
    machine_id: String,
}

/// What the task's last run left to resume.
enum Preserved {
    /// No candidate, and why.
    None(Fresh),
    /// A candidate that must not be resumed, and why.
    Refused(Candidate, Fresh),
    Usable(Candidate),
}

/// Why the implementer starts from scratch: a stable `code` for consumers and
/// the operator-facing detail.
struct Fresh {
    code: &'static str,
    detail: String,
}

impl Fresh {
    fn new(code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
        }
    }
}

enum Outcome {
    Fresh(Fresh),
    Validated,
    /// [ORB-14450] A held candidate applied cleanly; review decides.
    Held,
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
    tracing::info!(
        task_id,
        outcome = outcome_name(&outcome),
        source_run_id = %candidate.run_id,
        "claimed candidate resume"
    );
    Ok(output(&outcome, Some(&candidate), base_sha))
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
        durable_ref: None,
        failed_step_id: checkpoint.failed_step_id,
        needs_review_repair: evidence["decision"] == "blocked_review_gate",
        held: false,
        claim: None,
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
    tracing::info!(
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

/// What squash-merging a candidate onto a clean base checkout left behind.
enum Applied {
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
fn apply(
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
fn resume<H: RuntimeHost + ?Sized>(
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
fn record<H: RuntimeHost + ?Sized>(
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
    tracing::info!(task_id = %task.id, run_id, outcome = outcome_name(outcome), "candidate resume");
    Ok(())
}

fn outcome_name(outcome: &Outcome) -> &'static str {
    match outcome {
        Outcome::Fresh(_) => "fresh",
        Outcome::Validated => "resumed_validated",
        Outcome::Held => "resumed_held",
        Outcome::Unjudged(_) => "resumed_unjudged",
        Outcome::Repair(_) => "resumed_repaired",
    }
}

fn output(outcome: &Outcome, candidate: Option<&Candidate>, base_sha: &str) -> Value {
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
