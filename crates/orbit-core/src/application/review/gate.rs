//! The before-PR review gate: admit a fresh reviewer, then settle its
//! report into an honest verdict and certificate [ORB-11333].

use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::Utc;
use orbit_automation::review::{
    combined_task_meaning_digest, task_meaning_digest, validation_evidence, validation_role_counts,
};
use orbit_common::OrbitError;
use orbit_common::fs::selector::overlaps;
use orbit_engine::DispatchError;
use orbit_engine::review_gate::{
    CandidateIdentity, candidate_identity, commit_reviewer_repairs, uncommitted_paths,
};
use orbit_store::contracts::{ReviewReserveRequest, ReviewSettlement, ReviewStoreBackend};
use orbit_types::identity::Crew;
use orbit_types::task::{Task, TaskArtifact};
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::workflow::automation::SourceRevision;
use orbit_types::workflow::{
    CommitIdentity, FindingDisposition, REVIEW_CONTRACT_VERSION, REVIEW_GATE_ARTIFACT,
    REVIEW_MANIFEST_ARTIFACT, REVIEW_REPORT_ARTIFACT, ReviewAdmission, ReviewAttempt,
    ReviewAttemptState, ReviewCertificate, ReviewLedger, ReviewManifest, ReviewReport,
    ReviewReservation, ReviewVerdict, ReviewerIdentity,
};
use serde_json::{Value, json};

use super::admission::run_review_admission;
use super::{REVIEW_AUDIT, automation_error, lineage_key};
use crate::OrbitRuntime;
use crate::application::automation::source::Source;
use crate::application::task::TaskUpdateParams;
use crate::runtime::engine::crew::enforce_crew_allowlist;

/// Admit a reviewer for the committed, base-synchronized candidate.
///
/// Output always carries `applies`; when the gate does not apply the pipeline
/// continues to publish without a review. When it applies, the output pins
/// the candidate, names the reviewer, and records the attempt the settle
/// step must close.
pub(crate) fn review_gate_admit(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
) -> Result<Value, DispatchError> {
    let failed = |message: String| DispatchError::DeterministicActionFailed {
        action: action.to_string(),
        message,
    };
    // A run without a captured review admission predates the policy or was
    // never a delivery submission: it keeps the pre-existing behavior and
    // never loads tasks or Git state for a gate that cannot apply.
    // Prefer the dispatcher-injected `run_id` (the admitted job) over
    // `job_run_id`, which an epic pipeline may still spell as the stable
    // worktree token that has no run record [ORB-11520].
    let run_id = admitted_run_id(input).map_err(|error| failed(error.to_string()))?;
    let Some(admission) =
        run_review_admission(runtime, &run_id).map_err(|error| failed(error.to_string()))?
    else {
        return Ok(not_applicable("review_admission_missing", None));
    };
    if !admission.gates_pr() {
        let reason = match admission.timing {
            orbit_types::workflow::ReviewTiming::None => "review_policy_none",
            orbit_types::workflow::ReviewTiming::AfterLanding => "review_policy_after_landing",
            orbit_types::workflow::ReviewTiming::BeforePr => unreachable!("gates_pr"),
        };
        return Ok(not_applicable(reason, Some(&admission)));
    }
    if input
        .get("skipped_no_diff_expected")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        // A checked no-diff exemption delivers no code: nothing to review and
        // nothing that could later count as code coverage.
        return Ok(not_applicable("no_diff_exemption", Some(&admission)));
    }
    if input.get("mode").and_then(Value::as_str) == Some("local") {
        // V1 rejects `before-pr` on a local-only route instead of changing
        // what the policy means; the epic pipeline learns its route late.
        return Err(failed(
            "review_policy_local_route_refused: this run carries a before-pr review admission \
             but delivers locally; ship through the PR route or choose none/after-landing"
                .to_string(),
        ));
    }

    let context = GateContext::load(runtime, input).map_err(|error| failed(error.to_string()))?;
    let outcome = admit(runtime, &context, &admission);
    let audit_args = json!({
        "phase": "admit",
        "run_id": context.run_id,
        "task_ids": context.task_ids,
        "reviewer_crew": admission.crew,
        "outcome": outcome.as_ref().map(|value| value["decision"].clone()).unwrap_or(json!("refused")),
        "recorded_at": Utc::now().to_rfc3339(),
    });
    runtime
        .record_pipeline_audit(
            REVIEW_AUDIT,
            Some(&context.run_id),
            Some("system"),
            if outcome.is_ok() {
                AuditEventStatus::Success
            } else {
                AuditEventStatus::Failure
            },
            audit_args,
            outcome.as_ref().err().map(ToString::to_string),
        )
        .map_err(|error| failed(error.to_string()))?;
    outcome.map_err(|error| failed(error.to_string()))
}

/// Close the admitted attempt with an honest verdict.
///
/// A pass returns the reviewed head and base the PR steps must recheck; a
/// non-pass fails the step so the pipeline's failure handoff preserves the
/// candidate and blocks the task with the escalation.
pub(crate) fn review_gate_settle(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
) -> Result<Value, DispatchError> {
    let failed = |message: String| DispatchError::DeterministicActionFailed {
        action: action.to_string(),
        message,
    };
    let admission_output = input.get("admission").cloned().unwrap_or(Value::Null);
    if admission_output.get("applies").and_then(Value::as_bool) != Some(true) {
        return Ok(json!({
            "gate": "not_required",
            "reason": admission_output.get("reason").cloned().unwrap_or(Value::Null),
            "reviewed_head_sha": "",
            "reviewed_base_sha": "",
        }));
    }
    let mut settle_input = input.clone();
    if let (Some(object), Some(base_sha)) = (
        settle_input.as_object_mut(),
        admission_output.get("base_sha").cloned(),
    ) {
        object.insert("base_sha".to_string(), base_sha);
    }
    let context =
        GateContext::load(runtime, &settle_input).map_err(|error| failed(error.to_string()))?;
    if context.admission.is_none() {
        return Err(failed(
            "review_gate_stale: the run no longer carries a review admission".to_string(),
        ));
    }
    let attempt_id = admission_output
        .get("attempt_id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| failed("review_gate_settle requires admission.attempt_id".to_string()))?
        .to_string();
    let reviewer = reviewer_identity(runtime, &context, &admission_output)
        .map_err(|error| failed(error.to_string()))?;

    let outcome = settle(runtime, &context, &attempt_id, reviewer, &admission_output);
    let (status, decision, error) = match &outcome {
        Ok(Settled::Passed(value)) => (AuditEventStatus::Success, value.clone(), None),
        Ok(Settled::Blocked { certificate }) => (
            AuditEventStatus::Failure,
            json!({
                "verdict": certificate.verdict.as_str(),
                "escalation": certificate.escalation,
            }),
            None,
        ),
        Err(error) => (
            AuditEventStatus::Failure,
            json!("refused"),
            Some(error.to_string()),
        ),
    };
    runtime
        .record_pipeline_audit(
            REVIEW_AUDIT,
            Some(&context.run_id),
            Some("system"),
            status,
            json!({
                "phase": "settle",
                "run_id": context.run_id,
                "task_ids": context.task_ids,
                "attempt_id": attempt_id,
                "outcome": decision,
                "recorded_at": Utc::now().to_rfc3339(),
            }),
            error,
        )
        .map_err(|error| failed(error.to_string()))?;

    match outcome {
        Ok(Settled::Passed(value)) => Ok(value),
        Ok(Settled::Blocked { certificate }) => Err(failed(format!(
            "review_gate_blocked: verdict {} ({}); {} finding(s) recorded; the candidate stays \
             unpublished until a recorded decision resumes delivery",
            certificate.verdict.as_str(),
            certificate
                .escalation
                .as_deref()
                .unwrap_or("no escalation reason recorded"),
            certificate.findings.len()
        ))),
        Err(error) => Err(failed(error.to_string())),
    }
}

/// Shared inputs of both gate steps.
struct GateContext {
    run_id: String,
    task_ids: Vec<String>,
    tasks: Vec<Task>,
    workspace_path: PathBuf,
    /// The synchronized base the candidate sits on, pinned at admission.
    base_sha: Option<String>,
    base_branch: String,
    base_sync: String,
    admission: Option<ReviewAdmission>,
    workspace_id: String,
    repository: String,
}

impl GateContext {
    fn load(runtime: &OrbitRuntime, input: &Value) -> Result<Self, OrbitError> {
        let run_id = admitted_run_id(input)?;
        let task_ids = input
            .get("completed_task_ids")
            .and_then(Value::as_array)
            .map(|ids| {
                ids.iter()
                    .filter_map(Value::as_str)
                    .map(str::trim)
                    .filter(|id| !id.is_empty())
                    .map(ToOwned::to_owned)
                    .collect::<Vec<_>>()
            })
            .filter(|ids| !ids.is_empty())
            .ok_or_else(|| {
                OrbitError::InvalidInput(
                    "review gate requires input.completed_task_ids".to_string(),
                )
            })?;
        let workspace_path = PathBuf::from(required_string(input, "workspace_path")?)
            .canonicalize()
            .map_err(|error| {
                OrbitError::InvalidInput(format!("review gate workspace_path: {error}"))
            })?;
        let base_sha = input
            .get("base_sha")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);
        let base_sync = input
            .get("base_sync")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("remote")
            .to_string();
        let base_branch = input
            .get("base")
            .and_then(Value::as_str)
            .map(|base| base.trim().trim_start_matches("origin/").to_string())
            .filter(|base| !base.is_empty())
            .unwrap_or_else(|| "main".to_string());

        let mut tasks = Vec::with_capacity(task_ids.len());
        for task_id in &task_ids {
            let task = runtime.get_task(task_id)?;
            if task.job_run_id.as_deref() != Some(run_id.as_str()) {
                return Err(OrbitError::Execution(format!(
                    "review gate: task '{task_id}' no longer belongs to run '{run_id}'"
                )));
            }
            tasks.push(task);
        }
        let admission = run_review_admission(runtime, &run_id)?;
        let repository = Source::new(&runtime.paths().repo_root)
            .repository()
            .map_err(automation_error)?;
        Ok(Self {
            run_id,
            task_ids,
            tasks,
            workspace_path,
            base_sha,
            base_branch,
            base_sync,
            admission,
            workspace_id: runtime.workspace_id()?,
            repository,
        })
    }

    /// The base commit the candidate is pinned against: the explicit pin when
    /// a step supplied one, otherwise the merge base with the synchronized
    /// base ref.
    fn base_sha(&self) -> Result<String, OrbitError> {
        match &self.base_sha {
            Some(base_sha) => Ok(base_sha.clone()),
            None => Ok(orbit_engine::review_gate::synchronized_base(
                &self.workspace_path,
                &self.base_branch,
                &self.base_sync,
            )?
            .commit),
        }
    }

    fn lineage_key(&self) -> String {
        lineage_key(&self.workspace_id, &self.task_ids, &self.base_branch)
    }

    fn task_digests(&self) -> Result<(BTreeMap<String, String>, String), OrbitError> {
        let mut digests = BTreeMap::new();
        for task in &self.tasks {
            digests.insert(
                task.id.to_string(),
                task_meaning_digest(task).map_err(automation_error)?,
            );
        }
        let combined = combined_task_meaning_digest(
            &digests
                .iter()
                .map(|(id, digest)| (id.clone(), digest.clone()))
                .collect::<Vec<_>>(),
        )
        .map_err(automation_error)?;
        Ok((digests, combined))
    }
}

fn required_string(input: &Value, key: &str) -> Result<String, OrbitError> {
    input
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| OrbitError::InvalidInput(format!("review gate requires input.{key}")))
}

/// The admitted job that captured review policy and owns the candidate.
///
/// Dispatcher injects the executing run as `run_id`. Epic pipelines may pass
/// the stable worktree token as `job_run_id`; that token is not a run record.
fn admitted_run_id(input: &Value) -> Result<String, OrbitError> {
    let job_run_id = required_string(input, "job_run_id")?;
    let injected = input
        .get("run_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    Ok(injected.unwrap_or(job_run_id))
}

fn not_applicable(reason: &str, admission: Option<&ReviewAdmission>) -> Value {
    json!({
        "applies": false,
        "reason": reason,
        "timing": admission.map(|admission| admission.timing.as_str()),
        "timing_source": admission.map(|admission| admission.timing_source.clone()),
    })
}

fn admit(
    runtime: &OrbitRuntime,
    context: &GateContext,
    admission: &ReviewAdmission,
) -> Result<Value, OrbitError> {
    let crew = resolve_reviewer_crew(runtime, admission, context)?;
    let candidate = candidate_identity(&context.workspace_path, &context.base_sha()?)?;
    if candidate.commits.is_empty() {
        return Err(OrbitError::Execution(format!(
            "review_gate_admit: candidate {} adds no commits over base {}; nothing to review",
            candidate.head.commit, candidate.base.commit
        )));
    }
    let (task_digests, task_meaning_digest) = context.task_digests()?;

    let store = runtime.review_store()?;
    let lineage_key = context.lineage_key();
    let now = Utc::now();
    let (reservation, ledger) = store.review_reserve(
        &context.workspace_id,
        &ReviewReserveRequest {
            lineage_key: &lineage_key,
            task_ids: &context.task_ids,
            run_id: &context.run_id,
            task_meaning_digest: &task_meaning_digest,
            candidate: &candidate.head,
            budget: admission.budget,
            now,
        },
    )?;
    let (attempt, resumed) = match reservation {
        ReviewReservation::Reserved { attempt } => (attempt, false),
        ReviewReservation::Resumed { attempt } => (attempt, true),
        ReviewReservation::Exhausted { reason, consumed } => {
            return Err(OrbitError::Execution(format!(
                "review_budget_exhausted: {reason} for lineage '{lineage_key}' (reviewer starts \
                 {}/{}, repair cycles {}/{}, {}s of {}s); a recorded decision must reset or \
                 re-scope this candidate before another review",
                consumed.reviewer_starts,
                ledger.budget.reviewer_starts,
                consumed.repair_cycles,
                ledger.budget.repair_cycles,
                consumed.seconds,
                u64::from(ledger.budget.minutes) * 60
            )));
        }
    };

    let manifest = ReviewManifest {
        schema_version: REVIEW_CONTRACT_VERSION,
        attempt_id: attempt.attempt_id.clone(),
        lineage_key: lineage_key.clone(),
        task_ids: context.task_ids.clone(),
        task_digests,
        task_meaning_digest,
        repository: context.repository.clone(),
        base: candidate.base.clone(),
        candidate: candidate.head.clone(),
        implementation_commits: candidate.commits.clone(),
        implementer_summaries: context
            .tasks
            .iter()
            .map(|task| (task.id.to_string(), task.execution_summary.clone()))
            .collect(),
        reviewer_crew: crew.name.clone(),
        contract_version: REVIEW_CONTRACT_VERSION,
        policy_version: admission.policy_version,
        budget: ledger.budget,
        remaining: ledger.remaining_at(now),
        issued_at: now,
    };
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|error| OrbitError::Execution(format!("serialize review manifest: {error}")))?;
    let selectors = context
        .tasks
        .iter()
        .map(|task| (task.id.to_string(), task.context_files.clone()))
        .collect::<BTreeMap<_, _>>();
    for task in &context.tasks {
        write_artifact(
            runtime,
            &task.id,
            &context.run_id,
            REVIEW_MANIFEST_ARTIFACT,
            manifest_bytes.clone(),
        )?;
    }

    Ok(json!({
        "applies": true,
        "decision": if resumed { "resumed" } else { "admitted" },
        "first_task_id": context.task_ids[0],
        "attempt_id": attempt.attempt_id,
        "attempt_index": attempt.index,
        "lineage_key": lineage_key,
        "timing": admission.timing.as_str(),
        "timing_source": admission.timing_source,
        "reviewer": reviewer_json(&crew, &admission.crew_source),
        "base_sha": candidate.base.commit,
        "base_tree": candidate.base.tree,
        "head_sha": candidate.head.commit,
        "head_tree": candidate.head.tree,
        "implementation_commit_count": candidate.commits.len(),
        "task_meaning_digest": manifest.task_meaning_digest,
        "task_selectors": selectors,
        "manifest_artifact": REVIEW_MANIFEST_ARTIFACT,
        "report_artifact": REVIEW_REPORT_ARTIFACT,
        "budget": ledger.budget,
        "remaining": ledger.remaining_at(now),
        "started_at": attempt.started_at.to_rfc3339(),
    }))
}

/// The reviewer crew must be explicitly configured, resolvable on this host,
/// and inside the run's crew allowlist. It is never inferred from the
/// implementer; refusal escalates rather than substituting.
fn resolve_reviewer_crew(
    runtime: &OrbitRuntime,
    admission: &ReviewAdmission,
    context: &GateContext,
) -> Result<Crew, OrbitError> {
    let name = admission.crew.as_deref().ok_or_else(|| {
        OrbitError::CapabilityDenied(
            "review_crew_unconfigured: before-pr review needs an explicitly configured \
             operation.review_crew; automatic review never inherits the implementer's crew"
                .to_string(),
        )
    })?;
    let crew = runtime
        .resolve_crew_for_task(Some(name), None)
        .map_err(|error| {
            OrbitError::CapabilityDenied(format!(
                "review_crew_unavailable: configured review crew '{name}' cannot be resolved on \
                 this host: {error}"
            ))
        })?;
    let run = runtime.get_job_run_backend(&context.run_id)?;
    let run_input = run.and_then(|run| run.input).unwrap_or(Value::Null);
    let allowlist = runtime.crew_allowlist_from_input(&run_input)?;
    enforce_crew_allowlist(allowlist.as_ref(), &crew, "the configured review crew").map_err(
        |error| {
            OrbitError::CapabilityDenied(format!(
                "review_crew_excluded: {error}; the gate escalates rather than substituting a \
                 crew the run's window excluded"
            ))
        },
    )?;
    Ok(crew)
}

fn reviewer_json(crew: &Crew, source: &str) -> Value {
    json!({
        "crew": crew.name,
        "crew_source": source,
        "provider": crew.assignment.provider,
        "model": crew.assignment.model,
        "reasoning_effort": crew.assignment.effort,
    })
}

fn reviewer_identity(
    runtime: &OrbitRuntime,
    context: &GateContext,
    admission_output: &Value,
) -> Result<ReviewerIdentity, OrbitError> {
    let reviewer = admission_output
        .get("reviewer")
        .cloned()
        .unwrap_or(Value::Null);
    let field = |key: &str| {
        reviewer
            .get(key)
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .ok_or_else(|| {
                OrbitError::InvalidInput(format!(
                    "review_gate_settle requires admission.reviewer.{key}"
                ))
            })
    };
    let implementer_model = runtime
        .get_job_run_backend(&context.run_id)?
        .and_then(|run| run.crew_model)
        .map(|model| model.trim().to_string())
        .filter(|model| !model.is_empty());
    let model = field("model")?;
    Ok(ReviewerIdentity {
        crew: field("crew")?,
        provider: field("provider")?,
        same_model_as_implementer: implementer_model.as_deref() == Some(model.as_str()),
        model,
        reasoning_effort: reviewer
            .get("reasoning_effort")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        implementer_model,
    })
}

enum Settled {
    Passed(Value),
    Blocked { certificate: Box<ReviewCertificate> },
}

fn settle(
    runtime: &OrbitRuntime,
    context: &GateContext,
    attempt_id: &str,
    reviewer: ReviewerIdentity,
    admission_output: &Value,
) -> Result<Settled, OrbitError> {
    let store = runtime.review_store()?;
    let lineage_key = context.lineage_key();
    let ledger = store
        .review_ledger(&context.workspace_id, &lineage_key)?
        .ok_or_else(|| {
            OrbitError::Execution(format!(
                "review_gate_stale: lineage '{lineage_key}' has no ledger for attempt {attempt_id}"
            ))
        })?;
    let attempt = ledger
        .attempts
        .iter()
        .find(|attempt| attempt.attempt_id == attempt_id)
        .cloned()
        .ok_or_else(|| {
            OrbitError::Execution(format!(
                "review_gate_stale: attempt {attempt_id} is not part of lineage '{lineage_key}'"
            ))
        })?;

    // A replay after the attempt already settled reconciles the recorded
    // certificate instead of judging the candidate twice.
    if let ReviewAttemptState::Settled { .. } = attempt.state {
        return reconcile_settled(context, store.as_ref(), attempt_id);
    }

    let reviewed = candidate_identity(&context.workspace_path, &context.base_sha()?)?;
    if reviewed.head.commit != attempt.candidate.commit {
        return Err(OrbitError::Execution(format!(
            "review_gate_stale: candidate_changed: the worktree head is {} but attempt \
             {attempt_id} admitted {}; only one worker may settle a candidate",
            reviewed.head.commit, attempt.candidate.commit
        )));
    }

    let mut judgement = Judgement::from_report(runtime, context, &attempt)?;
    let admitted_selectors = admission_output
        .get("task_selectors")
        .cloned()
        .map(serde_json::from_value::<BTreeMap<String, Vec<String>>>)
        .transpose()
        .map_err(|error| {
            OrbitError::InvalidInput(format!("admission.task_selectors is unreadable: {error}"))
        })?
        .unwrap_or_default();
    judgement.check_task_meaning(context, &attempt, &admitted_selectors)?;
    let repair = judgement.commit_repairs(context, &reviewer, &attempt)?;
    judgement.reconcile_verdict(&ledger, repair.as_ref());
    let now = Utc::now();
    let elapsed_seconds = attempt.elapsed_at(now);
    judgement.enforce_wall_time(&ledger, elapsed_seconds);

    let final_candidate = match &repair {
        Some(commit) => SourceRevision {
            commit: commit.commit.clone(),
            tree: commit.tree.clone(),
        },
        None => reviewed.head.clone(),
    };
    let repair_cycles = u32::from(repair.is_some());
    let ledger = store.review_settle(
        &context.workspace_id,
        &ReviewSettlement {
            lineage_key: &lineage_key,
            attempt_id,
            verdict: judgement.verdict,
            repair_cycles,
            elapsed_seconds,
            now,
        },
    )?;

    let certificate = ReviewCertificate {
        schema_version: REVIEW_CONTRACT_VERSION,
        attempt_id: attempt_id.to_string(),
        lineage_key: lineage_key.clone(),
        task_ids: context.task_ids.clone(),
        task_meaning_digest: judgement.task_meaning_digest.clone(),
        repository: context.repository.clone(),
        base: reviewed.base.clone(),
        reviewed_candidate: reviewed.head.clone(),
        final_candidate: final_candidate.clone(),
        implementation_commits: reviewed.commits.clone(),
        repair_commits: repair.clone().into_iter().collect(),
        verdict: judgement.verdict,
        assurance: judgement.verdict.assurance(),
        findings: judgement.findings.clone(),
        validation: judgement.validation.clone(),
        validation_complete: judgement.validation_complete,
        reviewer,
        consumed: ledger.consumed(),
        budget: ledger.budget,
        escalation: judgement.escalation.clone(),
        issued_at: now,
    };
    store.review_certificate_record(&context.workspace_id, &certificate)?;
    let certificate_bytes = serde_json::to_vec_pretty(&certificate)
        .map_err(|error| OrbitError::Execution(format!("serialize review certificate: {error}")))?;
    for task in &context.tasks {
        write_artifact(
            runtime,
            &task.id,
            &context.run_id,
            REVIEW_GATE_ARTIFACT,
            certificate_bytes.clone(),
        )?;
        runtime.update_task(
            &task.id,
            TaskUpdateParams {
                comment: Some(verdict_comment(&certificate, &reviewed)),
                ..TaskUpdateParams::default()
            },
        )?;
    }

    if certificate.verdict.passed() {
        Ok(Settled::Passed(passed_output(&certificate)))
    } else {
        Ok(Settled::Blocked {
            certificate: Box::new(certificate),
        })
    }
}

fn reconcile_settled(
    context: &GateContext,
    store: &dyn ReviewStoreBackend,
    attempt_id: &str,
) -> Result<Settled, OrbitError> {
    let certificate = store
        .review_certificate(&context.workspace_id, attempt_id)?
        .ok_or_else(|| {
            OrbitError::Execution(format!(
                "review_gate_stale: attempt {attempt_id} settled without a recorded certificate; \
                 a fresh reviewer start is required"
            ))
        })?;
    if !certificate.verdict.passed() {
        return Ok(Settled::Blocked {
            certificate: Box::new(certificate),
        });
    }
    let head = orbit_engine::review_gate::revision(&context.workspace_path, "HEAD")?;
    if head != certificate.final_candidate {
        return Err(OrbitError::Execution(format!(
            "review_gate_stale: candidate_changed: the worktree head is {} but certificate \
             {attempt_id} settled {}",
            head.commit, certificate.final_candidate.commit
        )));
    }
    Ok(Settled::Passed(passed_output(&certificate)))
}

fn passed_output(certificate: &ReviewCertificate) -> Value {
    json!({
        "gate": "passed",
        "verdict": certificate.verdict.as_str(),
        "assurance": certificate.assurance.map(|assurance| assurance.as_str()),
        "attempt_id": certificate.attempt_id,
        "reviewed_head_sha": certificate.final_candidate.commit,
        "reviewed_base_sha": certificate.base.commit,
        "final_candidate_tree": certificate.final_candidate.tree,
        "implementation_commits": certificate.implementation_commits.iter().map(|c| &c.commit).collect::<Vec<_>>(),
        "repair_commits": certificate.repair_commits.iter().map(|c| &c.commit).collect::<Vec<_>>(),
        "findings": certificate.findings.len(),
        "consumed": certificate.consumed,
        "certificate_artifact": REVIEW_GATE_ARTIFACT,
    })
}

/// The reviewer's claims, checked against the repository and the budget.
struct Judgement {
    verdict: ReviewVerdict,
    findings: Vec<orbit_types::workflow::ReviewFinding>,
    validation: Vec<orbit_types::workflow::ReviewValidation>,
    validation_complete: bool,
    escalation: Option<String>,
    summary: String,
    task_meaning_digest: String,
}

impl Judgement {
    /// Read the report the reviewer persisted for this attempt. A missing,
    /// stale, or unreadable report is an incomplete review, never a pass.
    fn from_report(
        runtime: &OrbitRuntime,
        context: &GateContext,
        attempt: &ReviewAttempt,
    ) -> Result<Self, OrbitError> {
        let (_, task_meaning_digest) = context.task_digests()?;
        let incomplete = |reason: &str| Self {
            verdict: ReviewVerdict::Incomplete,
            findings: Vec::new(),
            validation: Vec::new(),
            validation_complete: false,
            escalation: Some(reason.to_string()),
            summary: String::new(),
            task_meaning_digest: task_meaning_digest.clone(),
        };
        let first_task = &context.task_ids[0];
        let Some(artifact) = runtime.get_task_artifact(first_task, REVIEW_REPORT_ARTIFACT)? else {
            return Ok(incomplete(
                "report_missing: the reviewer persisted no review-report.json",
            ));
        };
        let manifest = runtime.get_task_artifact_manifest(first_task)?;
        let provenance = manifest
            .iter()
            .find(|file| file.path == REVIEW_REPORT_ARTIFACT);
        if provenance.is_some_and(|file| file.created_at < attempt.started_at) {
            return Ok(incomplete(
                "report_stale: review-report.json predates this attempt",
            ));
        }
        let report: ReviewReport = match serde_json::from_slice(&artifact.content) {
            Ok(report) => report,
            Err(error) => {
                return Ok(incomplete(&format!("report_unreadable: {error}")));
            }
        };
        if report.schema_version != REVIEW_CONTRACT_VERSION {
            return Ok(incomplete(&format!(
                "report_contract_mismatch: schema_version {} is not {REVIEW_CONTRACT_VERSION}",
                report.schema_version
            )));
        }
        if report.attempt_id != attempt.attempt_id {
            return Ok(incomplete(&format!(
                "report_attempt_mismatch: report names attempt {} but {} was admitted",
                report.attempt_id, attempt.attempt_id
            )));
        }
        Ok(Self {
            verdict: report.verdict,
            findings: report.findings,
            validation: report.validation,
            validation_complete: false,
            escalation: report.escalation,
            summary: report.summary,
            task_meaning_digest,
        })
    }

    /// Task criteria, scope, or contract changes during the review
    /// invalidate it. A reviewer may add selectors for a coupled repair
    /// through the task API; anything else re-establishes review.
    fn check_task_meaning(
        &mut self,
        context: &GateContext,
        attempt: &ReviewAttempt,
        admitted_selectors: &BTreeMap<String, Vec<String>>,
    ) -> Result<(), OrbitError> {
        if self.task_meaning_digest == attempt.task_meaning_digest {
            return Ok(());
        }
        if !selectors_only_grew(context, attempt, admitted_selectors)? {
            self.downgrade(
                "task_meaning_changed: task criteria, plan, scope, or relations changed \
                 during the review",
            );
        }
        Ok(())
    }

    /// Commit whatever the reviewer changed as its own attributed work.
    fn commit_repairs(
        &mut self,
        context: &GateContext,
        reviewer: &ReviewerIdentity,
        attempt: &ReviewAttempt,
    ) -> Result<Option<CommitIdentity>, OrbitError> {
        let changed = uncommitted_paths(&context.workspace_path)?;
        if changed.is_empty() {
            return Ok(None);
        }
        let out_of_scope = changed
            .iter()
            .filter(|path| !path_in_scope(path, &context.tasks))
            .cloned()
            .collect::<Vec<_>>();
        let finding_ids = self
            .findings
            .iter()
            .filter(|finding| finding.disposition == FindingDisposition::Repaired)
            .map(|finding| finding.id.clone())
            .collect::<Vec<_>>();
        let message = format!(
            "review: {} [{}]\n\nFindings: {}\nPaths: {}\nOrbit-Review-Attempt: {}\nOrbit-Review-Crew: {}",
            if self.summary.trim().is_empty() {
                "reviewer repairs".to_string()
            } else {
                self.summary
                    .trim()
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_string()
            },
            context.task_ids.join(", "),
            if finding_ids.is_empty() {
                "none named".to_string()
            } else {
                finding_ids.join(", ")
            },
            changed.join(", "),
            attempt.attempt_id,
            reviewer.crew,
        );
        // The provider names the agent family the repair commit is attributed
        // to; the model alone may carry no family hint.
        let reviewer_label = format!("{} / {}", reviewer.provider, reviewer.model);
        let commit = commit_reviewer_repairs(&context.workspace_path, &reviewer_label, &message)?;
        if !out_of_scope.is_empty() {
            self.downgrade(&format!(
                "repair_out_of_scope: reviewer changed paths outside the task scope: {}",
                out_of_scope.join(", ")
            ));
        }
        Ok(commit)
    }

    /// Cross-check the claimed verdict against what actually happened.
    fn reconcile_verdict(&mut self, ledger: &ReviewLedger, repair: Option<&CommitIdentity>) {
        let open_findings = self
            .findings
            .iter()
            .filter(|finding| finding.disposition == FindingDisposition::Open)
            .count();
        match self.verdict {
            ReviewVerdict::PassedWithoutRepairs if repair.is_some() => self.downgrade(
                "verdict_inconsistent: the reviewer reported no repairs but changed the worktree",
            ),
            ReviewVerdict::PassedWithRepairs if repair.is_none() => self.downgrade(
                "verdict_inconsistent: the reviewer reported repairs but changed nothing",
            ),
            ReviewVerdict::PassedWithRepairs if ledger.remaining().repair_cycles == 0 => self
                .downgrade(
                    "review_repair_cycles_exhausted: the lineage has no repair cycle left for \
                     these reviewer repairs",
                ),
            ReviewVerdict::PassedWithoutRepairs | ReviewVerdict::PassedWithRepairs
                if open_findings > 0 =>
            {
                self.downgrade(&format!(
                    "verdict_inconsistent: {open_findings} finding(s) remain open under a pass"
                ));
            }
            ReviewVerdict::ChangesRequired if self.escalation.is_none() => {
                self.escalation = Some("changes_required".to_string());
            }
            _ => {}
        }
        // A pass rests on what the records establish, not on their count:
        // a required check must have passed, while a declared negative
        // control, an excluded action, and a superseded attempt carry their
        // own consistency rules. Delivery coverage reads the same function.
        if self.verdict.passed() {
            match validation_evidence(&self.validation) {
                Ok(()) => self.validation_complete = true,
                Err(defect) => self.downgrade(&defect.reason()),
            }
        }
    }

    /// A pass may not spend more wall time than the captured lineage budget,
    /// including this attempt's elapsed seconds. Non-pass verdicts still
    /// record the honest elapsed time.
    fn enforce_wall_time(&mut self, ledger: &ReviewLedger, elapsed_seconds: u64) {
        if !self.verdict.passed() {
            return;
        }
        let budget_seconds = u64::from(ledger.budget.minutes).saturating_mul(60);
        if ledger.consumed_seconds.saturating_add(elapsed_seconds) > budget_seconds {
            self.downgrade(
                "review_minutes_exhausted: this attempt exceeded the captured lineage \
                 wall-time allowance",
            );
        }
    }

    fn downgrade(&mut self, reason: &str) {
        self.verdict = ReviewVerdict::Incomplete;
        self.validation_complete = false;
        self.escalation = Some(match self.escalation.take() {
            Some(existing) if !existing.is_empty() => format!("{existing}; {reason}"),
            _ => reason.to_string(),
        });
    }
}

/// Whether every task still means what was admitted except for selectors
/// the reviewer added through the task API: restoring the admitted
/// selectors must reproduce the admitted digest, and the current selectors
/// must contain every admitted one.
fn selectors_only_grew(
    context: &GateContext,
    attempt: &ReviewAttempt,
    admitted_selectors: &BTreeMap<String, Vec<String>>,
) -> Result<bool, OrbitError> {
    let mut digests = Vec::with_capacity(context.tasks.len());
    for task in &context.tasks {
        let Some(admitted) = admitted_selectors.get(task.id.as_str()) else {
            return Ok(false);
        };
        if admitted
            .iter()
            .any(|selector| !task.context_files.contains(selector))
        {
            return Ok(false);
        }
        let mut restored = task.clone();
        restored.context_files = admitted.clone();
        digests.push((
            task.id.to_string(),
            task_meaning_digest(&restored).map_err(automation_error)?,
        ));
    }
    let restored_digest = combined_task_meaning_digest(&digests).map_err(automation_error)?;
    Ok(restored_digest == attempt.task_meaning_digest)
}

/// A repair path is in scope when a task selector's filesystem anchor names
/// it or a directory/legacy selector contains it. Matching uses the shared
/// selector grammar, so `symbol:<path>#<symbol>:<kind>` authorizes the
/// backing file even when `<symbol>` contains `::`.
fn path_in_scope(path: &str, tasks: &[Task]) -> bool {
    let path = path.trim_start_matches("./");
    let changed = format!("file:{path}");
    tasks.iter().any(|task| {
        task.context_files
            .iter()
            .any(|selector| overlaps(selector, &changed))
    })
}

fn verdict_comment(certificate: &ReviewCertificate, reviewed: &CandidateIdentity) -> String {
    let assurance = certificate
        .assurance
        .map(|assurance| assurance.as_str().to_string())
        .unwrap_or_else(|| "none".to_string());
    let repairs = if certificate.repair_commits.is_empty() {
        "none".to_string()
    } else {
        certificate
            .repair_commits
            .iter()
            .map(|commit| format!("`{}` by {}", commit.commit, commit.author))
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!(
        "before-PR review gate settled attempt `{}`: verdict **{}** (assurance: {}).\n\n\
         - Reviewer: crew `{}` ({} / {}){}\n\
         - Reviewed candidate: `{}` on base `{}` ({} implementation commit(s))\n\
         - Final candidate: `{}`\n\
         - Reviewer repair commits: {}\n\
         - Findings: {} ({} open)\n\
         - Validation on final candidate: {} record(s) [{}], complete: {}\n\
         - Consumed: {} reviewer start(s), {} repair cycle(s), {}s of {} min\n\
         - Escalation: {}\n\n\
         Reviewer repairs were validated but not independently reviewed; this verdict is \
         review evidence, not task approval or merge permission.",
        certificate.attempt_id,
        certificate.verdict.as_str(),
        assurance,
        certificate.reviewer.crew,
        certificate.reviewer.provider,
        certificate.reviewer.model,
        if certificate.reviewer.same_model_as_implementer {
            "; same model as the implementer, reported as such"
        } else {
            ""
        },
        reviewed.head.commit,
        reviewed.base.commit,
        reviewed.commits.len(),
        certificate.final_candidate.commit,
        repairs,
        certificate.findings.len(),
        certificate
            .findings
            .iter()
            .filter(|finding| finding.disposition == FindingDisposition::Open)
            .count(),
        certificate.validation.len(),
        validation_roles(&certificate.validation),
        certificate.validation_complete,
        certificate.consumed.reviewer_starts,
        certificate.consumed.repair_cycles,
        certificate.consumed.seconds,
        certificate.budget.minutes,
        certificate.escalation.as_deref().unwrap_or("none"),
    )
}

/// The classification breakdown of a validation set, so a reader sees which
/// records were required checks and which were controls or exclusions
/// without opening the certificate.
fn validation_roles(records: &[orbit_types::workflow::ReviewValidation]) -> String {
    let counts = validation_role_counts(records);
    if counts.is_empty() {
        return "none".to_string();
    }
    counts
        .into_iter()
        .map(|(role, count)| format!("{count} {}", role.as_str()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Write a gate artifact under the executor run's authority.
fn write_artifact(
    runtime: &OrbitRuntime,
    task_id: &str,
    run_id: &str,
    path: &str,
    content: Vec<u8>,
) -> Result<(), OrbitError> {
    runtime.update_task_with_owner(
        task_id,
        TaskUpdateParams {
            upsert_artifacts: vec![TaskArtifact {
                path: path.to_string(),
                content,
                media_type: "application/json".to_string(),
                created_by: Some("system".to_string()),
            }],
            ..TaskUpdateParams::default()
        },
        None,
        None,
        Some(run_id.to_string()),
    )?;
    Ok(())
}
