//! The deterministic steps of `task_review_reconciliation_pipeline`.
//!
//! Every step authenticates its run first: the run must be this job, carry
//! the reserved admission the governed submission stamped, and be the run
//! bound to the record's current attempt. A step input naming another
//! reconciliation, task or attempt is refused. Results are recorded on the
//! reconciliation as they are produced, so an interrupted attempt is
//! replaced by a new one that keeps what was already recorded.

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::protocol::tool_input::required_string;
use orbit_common::security::release::sha256_hex;
use orbit_engine::review_gate::{self, RequiredValidationRun};
use orbit_types::task::{TaskArtifact, TaskComplexity, TaskRelation, TaskRelationType, TaskType};
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::workflow::{
    FindingDisposition, REVIEW_RECONCILIATION_JOB, ReconciledCommand, ReconciledCommandRun,
    ReconciledReview, ReconciledValidation, ReconciliationAdmission, ReconciliationLog,
    ReconciliationOutcome, ReviewReconciliation, ReviewReport, ReviewVerdict,
};
use serde_json::{Value, json};

use super::observe::observe;
use super::submit::bind_attempt_run;
use super::{FOLLOW_UP_TAG_PREFIX, RECONCILIATION_AUDIT};
use crate::OrbitRuntime;
use crate::application::task::{TaskAddParams, TaskListFilter, TaskUpdateParams};

/// An authenticated step of one admitted attempt.
struct Admitted {
    run_id: String,
    attempt: u32,
    record: ReviewReconciliation,
}

fn refused(message: impl Into<String>) -> OrbitError {
    OrbitError::InvalidInput(message.into())
}

fn authenticate(
    runtime: &OrbitRuntime,
    input: &Value,
    run_id: Option<&str>,
) -> Result<Admitted, OrbitError> {
    let run_id = run_id.ok_or_else(|| refused("a reconciliation step must run inside its run"))?;
    let run = runtime
        .get_job_run_backend(run_id)?
        .ok_or_else(|| refused(format!("run {run_id} is not recorded")))?;
    let admission = run
        .input
        .as_ref()
        .and_then(ReconciliationAdmission::from_run_input)
        .filter(|_| run.job_id == REVIEW_RECONCILIATION_JOB)
        .ok_or_else(|| {
            refused(
                "this run was not admitted by `orbit task reconcile-review submit`; a \
                 reconciliation cannot be produced from ordinary job input",
            )
        })?;
    let reconciliation_id = required_string(input, &["reconciliation_id"], "reconciliation_id")?;
    let task_id = required_string(input, &["task_id"], "task_id")?;
    let record = runtime
        .review_store()?
        .review_reconciliation(&runtime.workspace_id()?, &admission.reconciliation_id)?
        .filter(|record| {
            record.reconciliation_id == reconciliation_id && record.binding.task_id == task_id
        })
        .ok_or_else(|| {
            refused("the step names a different reconciliation than its run's admission")
        })?;
    let record = bind_attempt_run(
        runtime,
        &record.reconciliation_id,
        admission.attempt,
        run_id,
    )?;
    Ok(Admitted {
        run_id: run_id.to_string(),
        attempt: admission.attempt,
        record,
    })
}

/// `review_reconciliation_prepare`: re-observe the binding and hand the
/// reviewer its exact head.
pub(crate) fn prepare(
    runtime: &OrbitRuntime,
    input: &Value,
    run_id: Option<&str>,
) -> Result<Value, OrbitError> {
    let admitted = authenticate(runtime, input, run_id)?;
    let record = &admitted.record;
    if record.runs_settled() {
        return Err(refused(format!(
            "reconciliation {} already settled",
            record.reconciliation_id
        )));
    }
    let task_id = record.binding.task_id.clone();
    let observation = observe(runtime, &task_id)?;
    if observation.binding_digest != record.binding_digest {
        let record = refuse(
            runtime,
            admitted,
            "the delivery changed between submission and this run (task meaning, claim, \
             handoff, pull request or merged head)",
            "inspect it with `orbit task reconcile-review inspect` and submit a new request key \
             for the current head",
        )?;
        return Err(refused(format!(
            "reconciliation {} refused: the delivery changed since submission",
            record.reconciliation_id
        )));
    }
    // The reviewer runs on the crew frozen at submission, never on whatever
    // the owner's configuration names by now.
    let crew = record.contract.review_crew.clone();
    if let Err(error) = runtime.resolve_crew_for_task(Some(&crew), None) {
        return Err(refused(format!(
            "reconciliation {} reviews on crew '{crew}', frozen when it was submitted, which \
             cannot be resolved on this host: {error}",
            record.reconciliation_id
        )));
    }
    let pull_request = &record.binding.pull_request;
    Ok(json!({
        "task_id": task_id,
        "reconciliation_id": record.reconciliation_id,
        "head": pull_request.merged_head.commit,
        "base": pull_request.base.commit,
        "pull_request_url": pull_request.url,
        "workspace_path": runtime.paths().repo_root.to_string_lossy(),
        "crew": crew,
    }))
}

/// `review_reconciliation_validate`: run every required command at the
/// merged head and reproduce each failure at the base. Every outcome is
/// recorded; a failure never stops the others.
pub(crate) fn validate(
    runtime: &OrbitRuntime,
    input: &Value,
    run_id: Option<&str>,
) -> Result<Value, OrbitError> {
    let admitted = authenticate(runtime, input, run_id)?;
    if admitted.record.runs_settled() {
        return Err(refused("this reconciliation already settled"));
    }
    if let Some(validation) = &admitted.record.validation {
        return Ok(json!({"recorded": true, "complete": validation.complete}));
    }
    let binding = admitted.record.binding.clone();
    // The commands frozen at submission: a configuration edited since then
    // neither drops nor adds a requirement.
    let commands = admitted.record.contract.required_commands.clone();
    if commands.is_empty() {
        return Err(refused(
            "the reconciliation's frozen contract names no required validation command",
        ));
    }
    let head = &binding.pull_request.merged_head.commit;
    let base = &binding.pull_request.base.commit;
    let prefix = format!(
        "review-reconciliation/{}/attempt-{}",
        admitted.record.reconciliation_id, admitted.attempt
    );
    let head_runs = run_commands(runtime, &admitted.run_id, "head", head, &commands)?;
    let failed: Vec<String> = commands
        .iter()
        .zip(&head_runs)
        .filter(|(_, run)| !run.passed && run.failure_kind.as_deref() == Some("candidate"))
        .map(|(command, _)| command.clone())
        .collect();
    let base_runs = if failed.is_empty() {
        Vec::new()
    } else {
        run_commands(runtime, &admitted.run_id, "base", base, &failed)?
    };
    let mut recorded = Vec::new();
    for (index, (command, run)) in commands.iter().zip(&head_runs).enumerate() {
        let head_run = persist_run(
            runtime,
            &admitted,
            &format!("{prefix}/head-{index}.json"),
            head,
            run,
        )?;
        let baseline = match failed.iter().position(|failed| failed == command) {
            Some(position) => Some(persist_run(
                runtime,
                &admitted,
                &format!("{prefix}/base-{index}.json"),
                base,
                &base_runs[position],
            )?),
            None => None,
        };
        recorded.push(ReconciledCommand {
            command: command.clone(),
            head: head_run,
            baseline,
        });
    }
    let complete = recorded.iter().all(|command| command.head.passed);
    let validation = ReconciledValidation {
        run_id: admitted.run_id.clone(),
        commands: recorded,
        complete,
        recorded_at: Utc::now(),
    };
    let summary = json!({
        "recorded": true,
        "complete": complete,
        "failed": validation
            .commands
            .iter()
            .filter(|command| !command.head.passed)
            .map(|command| command.command.clone())
            .collect::<Vec<_>>(),
    });
    update(runtime, &admitted.record.reconciliation_id, |record| {
        if record.validation.is_none() {
            record.validation = Some(validation.clone());
        }
    })?;
    Ok(summary)
}

fn run_commands(
    runtime: &OrbitRuntime,
    run_id: &str,
    side: &str,
    commit: &str,
    commands: &[String],
) -> Result<Vec<RequiredValidationRun>, OrbitError> {
    let checkout_id = format!("{run_id}-{side}");
    let checkout = runtime.create_recovery_checkout(&checkout_id, commit)?;
    let runs = commands
        .iter()
        .map(|command| review_gate::run_required_validation(runtime, &checkout, command))
        .collect::<Result<Vec<_>, _>>();
    // The checkout is this run's own scratch; remove it whatever happened.
    let cleanup = runtime.remove_recovery_checkout(&checkout_id);
    let runs = runs?;
    cleanup?;
    Ok(runs)
}

fn persist_run(
    runtime: &OrbitRuntime,
    admitted: &Admitted,
    path: &str,
    commit: &str,
    run: &RequiredValidationRun,
) -> Result<ReconciledCommandRun, OrbitError> {
    let log = json!({
        "schema_version": 1,
        "reconciliation_id": admitted.record.reconciliation_id,
        "run_id": admitted.run_id,
        "tested_commit": commit,
        "command": run.command,
        "passed": run.passed,
        "exit_code": run.exit_code,
        "timed_out": run.timed_out,
        "failure_kind": run.failure_kind,
        "validation_env": run.environment,
        "output": run.output,
    });
    let log = write_artifact(runtime, admitted, path, &log)?;
    Ok(ReconciledCommandRun {
        commit: commit.to_string(),
        passed: run.passed,
        exit_code: Some(run.exit_code),
        failure_kind: run.failure_kind.clone(),
        log,
    })
}

fn write_artifact(
    runtime: &OrbitRuntime,
    admitted: &Admitted,
    path: &str,
    value: &Value,
) -> Result<ReconciliationLog, OrbitError> {
    let content = serde_json::to_vec_pretty(value)
        .map_err(|error| OrbitError::Execution(format!("encode {path}: {error}")))?;
    let sha256 = sha256_hex(&content);
    runtime.update_task_as_system(
        &admitted.record.binding.task_id,
        TaskUpdateParams {
            upsert_artifacts: vec![TaskArtifact {
                path: path.to_string(),
                content,
                media_type: "application/json".to_string(),
                created_by: None,
            }],
            ..TaskUpdateParams::default()
        },
        Some(admitted.run_id.clone()),
    )?;
    Ok(ReconciliationLog {
        path: path.to_string(),
        sha256,
    })
}

/// `review_reconciliation_settle`: record the reviewer's report, re-observe
/// the binding, decide the outcome, and file one follow-up for findings.
pub(crate) fn settle(
    runtime: &OrbitRuntime,
    input: &Value,
    run_id: Option<&str>,
) -> Result<Value, OrbitError> {
    let admitted = authenticate(runtime, input, run_id)?;
    if admitted.record.runs_settled() {
        return Ok(outcome_view(&admitted.record));
    }
    let validation = admitted
        .record
        .validation
        .clone()
        .ok_or_else(|| refused("validation was not recorded before settlement"))?;
    let review = match admitted.record.review.clone() {
        Some(review) => review,
        None => match reviewer_report(input, &admitted.record.reconciliation_id) {
            Ok(report) => {
                let path = format!(
                    "review-reconciliation/{}/attempt-{}/review.json",
                    admitted.record.reconciliation_id, admitted.attempt
                );
                let value = serde_json::to_value(&report)
                    .map_err(|error| OrbitError::Execution(format!("encode report: {error}")))?;
                let log = write_artifact(runtime, &admitted, &path, &value)?;
                let review = ReconciledReview {
                    run_id: admitted.run_id.clone(),
                    crew: admitted.record.contract.review_crew.clone(),
                    verdict: report.verdict,
                    summary: report.summary,
                    findings: report.findings,
                    report: log,
                    recorded_at: Utc::now(),
                };
                let recorded = review.clone();
                update(runtime, &admitted.record.reconciliation_id, |record| {
                    if record.review.is_none() {
                        record.review = Some(recorded.clone());
                    }
                })?;
                review
            }
            Err(reason) => {
                let record = refuse(
                    runtime,
                    admitted,
                    &format!("the reviewer returned no usable report: {reason}"),
                    "inspect the reconciliation run with `orbit run show <run>`, resolve the \
                     reviewer's problem, and submit a new request key",
                )?;
                return Ok(outcome_view(&record));
            }
        },
    };
    let task_id = admitted.record.binding.task_id.clone();
    match observe(runtime, &task_id) {
        Ok(observation) if observation.binding_digest == admitted.record.binding_digest => {}
        Ok(_) | Err(OrbitError::InvalidInput(_)) => {
            let record = refuse(
                runtime,
                admitted,
                "the delivery changed while the reconciliation ran",
                "inspect it with `orbit task reconcile-review inspect` and submit a new request \
                 key for the current head",
            )?;
            return Ok(outcome_view(&record));
        }
        Err(error) => return Err(error),
    }
    let decision = decide(&validation, &review);
    let follow_up = if decision.follow_up.is_empty() {
        None
    } else {
        Some(file_follow_up(
            runtime,
            &admitted.record,
            &decision.follow_up,
        )?)
    };
    let outcome = decision.outcome;
    let record = update(runtime, &admitted.record.reconciliation_id, |record| {
        if record.outcome.is_none() {
            record.outcome = Some(outcome.clone());
            record.follow_up_task_id = follow_up.clone();
        }
    })?;
    conclude(runtime, &record, &admitted.run_id)?;
    Ok(outcome_view(&record))
}

/// The reviewer's report from its step output: an envelope `result` with a
/// `report` naming this reconciliation.
fn reviewer_report(input: &Value, reconciliation_id: &str) -> Result<ReviewReport, String> {
    let output = input
        .get("review")
        .filter(|output| !output.is_null())
        .ok_or("the review step produced no output")?;
    if output
        .get("response_envelope_valid")
        .and_then(Value::as_bool)
        != Some(true)
    {
        return Err("the reviewer's response envelope was missing or invalid".into());
    }
    let report = output
        .get("report")
        .ok_or("the envelope result has no `report`")?;
    let bytes = match report {
        Value::String(text) => text.clone().into_bytes(),
        other => serde_json::to_vec(other).map_err(|error| error.to_string())?,
    };
    let report = ReviewReport::parse(&bytes)?;
    if report.attempt_id != reconciliation_id {
        return Err(format!(
            "the report names attempt `{}`, not this reconciliation",
            report.attempt_id
        ));
    }
    Ok(report)
}

struct Decision {
    outcome: ReconciliationOutcome,
    /// What the follow-up task must address; empty files none.
    follow_up: Vec<String>,
}

fn decide(validation: &ReconciledValidation, review: &ReconciledReview) -> Decision {
    let mut follow_up: Vec<String> = review
        .findings
        .iter()
        .filter(|finding| !matches!(finding.disposition, FindingDisposition::Disposed { .. }))
        .map(|finding| {
            format!(
                "[{}] {}{}",
                finding.severity,
                finding.summary,
                if finding.paths.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", finding.paths.join(", "))
                }
            )
        })
        .collect();
    let broken: Vec<&ReconciledCommand> = validation
        .commands
        .iter()
        .filter(|command| {
            !command.head.passed
                && command.head.failure_kind.as_deref() == Some("candidate")
                && command
                    .baseline
                    .as_ref()
                    .is_some_and(|baseline| baseline.passed)
        })
        .collect();
    follow_up.extend(broken.iter().map(|command| {
        format!(
            "`{}` fails at the merged head but passes at its base (log {})",
            command.command, command.head.log.path
        )
    }));
    let refused = |reason: String, next_step: &str| ReconciliationOutcome::Refused {
        reason,
        next_step: next_step.to_string(),
    };
    let open = review
        .findings
        .iter()
        .any(|finding| !matches!(finding.disposition, FindingDisposition::Disposed { .. }));
    let outcome = match review.verdict {
        ReviewVerdict::Accept if !open => None,
        ReviewVerdict::Accept | ReviewVerdict::Reject => Some(refused(
            format!("the reviewer rejected the merged head: {}", review.summary),
            "repair it through the follow-up task; the merged pull request is not rewritten",
        )),
        ReviewVerdict::AcceptWithFixes => Some(refused(
            format!(
                "the reviewer reported fixes, but a merged head cannot be repaired in review: {}",
                review.summary
            ),
            "land the fixes through the follow-up task; the merged pull request is not rewritten",
        )),
        ReviewVerdict::Incomplete => Some(refused(
            format!("the review could not complete: {}", review.summary),
            "resolve what stopped the reviewer and submit a new request key",
        )),
    };
    let outcome = outcome.unwrap_or_else(|| {
        if validation.complete {
            return ReconciliationOutcome::Accepted;
        }
        if let Some(command) = validation.commands.iter().find(|command| {
            !command.head.passed
                && command.head.failure_kind.as_deref() == Some("candidate")
                && command
                    .baseline
                    .as_ref()
                    .is_none_or(|baseline| !baseline.passed && baseline.failure_kind.as_deref() != Some("candidate"))
        }) {
            let baseline_log = command
                .baseline
                .as_ref()
                .map(|baseline| baseline.log.path.as_str())
                .unwrap_or("unavailable");
            return refused(
                format!(
                    "required validation `{}` fails at the merged head, but its base run did not establish whether the failure was already present (log {baseline_log})",
                    command.command
                ),
                "make the required check runnable at both revisions and submit a new request key",
            );
        }
        if let Some(command) = broken.first() {
            return refused(
                format!(
                    "required validation `{}` fails at the merged head and passes at its base",
                    command.command
                ),
                "repair it through the follow-up task; the merged pull request is not rewritten",
            );
        }
        if let Some(command) = validation.commands.iter().find(|command| {
            !command.head.passed && command.head.failure_kind.as_deref() != Some("candidate")
        }) {
            return refused(
                format!(
                    "required validation `{}` could not judge the merged head (a tool is \
                     missing from the validation environment; log {})",
                    command.command, command.head.log.path
                ),
                "make the tool available to the owner's validation environment and submit a \
                 new request key",
            );
        }
        ReconciliationOutcome::AwaitingDisposition {
            commands: validation
                .commands
                .iter()
                .filter(|command| command.reproduced_on_base())
                .map(|command| command.command.clone())
                .collect(),
        }
    });
    Decision { outcome, follow_up }
}

/// File (or find) the one follow-up task for this reconciliation.
fn file_follow_up(
    runtime: &OrbitRuntime,
    record: &ReviewReconciliation,
    items: &[String],
) -> Result<String, OrbitError> {
    if let Some(existing) = &record.follow_up_task_id {
        return Ok(existing.clone());
    }
    let tag = format!("{FOLLOW_UP_TAG_PREFIX}{}", record.reconciliation_id);
    let found = runtime.task_candidates(
        &TaskListFilter {
            tags: vec![tag.clone()],
            ..Default::default()
        },
        1,
    )?;
    if let Some(task) = found.items.first() {
        return Ok(task.id.clone());
    }
    let original = runtime.get_task(&record.binding.task_id)?;
    let pull_request = &record.binding.pull_request;
    let task = runtime.add_task(TaskAddParams {
        title: format!("Repair the merged head of: {}", original.title.trim()),
        description: format!(
            "Review reconciliation {} of pull request {} (merged head {}) found problems the \
             merged change carries. The merged pull request is not rewritten; land the repair \
             as new work.\n\n{}",
            record.reconciliation_id,
            pull_request.url,
            pull_request.merged_head.commit,
            items
                .iter()
                .map(|item| format!("- {item}"))
                .collect::<Vec<_>>()
                .join("\n")
        ),
        acceptance_criteria: items
            .iter()
            .map(|item| format!("Resolved on {}: {item}", pull_request.landing_branch))
            .collect(),
        relations: vec![TaskRelation {
            relation_type: TaskRelationType::SpawnedFrom,
            target: original.id.clone(),
        }],
        tags: vec![tag],
        complexity: TaskComplexity::Unassessed,
        task_type: Some(TaskType::Bug),
        system_created: true,
        ..TaskAddParams::default()
    })?;
    Ok(task.id)
}

fn refuse(
    runtime: &OrbitRuntime,
    admitted: Admitted,
    reason: &str,
    next_step: &str,
) -> Result<ReviewReconciliation, OrbitError> {
    let outcome = ReconciliationOutcome::Refused {
        reason: reason.to_string(),
        next_step: next_step.to_string(),
    };
    let record = update(runtime, &admitted.record.reconciliation_id, |record| {
        if record.outcome.is_none() {
            record.outcome = Some(outcome.clone());
        }
    })?;
    conclude(runtime, &record, &admitted.run_id)?;
    Ok(record)
}

/// Comment the settled outcome on the task and audit it.
fn conclude(
    runtime: &OrbitRuntime,
    record: &ReviewReconciliation,
    run_id: &str,
) -> Result<(), OrbitError> {
    let pull_request = &record.binding.pull_request;
    let outcome = record
        .outcome
        .as_ref()
        .map_or("unsettled", ReconciliationOutcome::as_str);
    let validation = record.validation.as_ref().map_or_else(
        || "not recorded".to_string(),
        |validation| {
            let passed = validation.commands.iter().filter(|c| c.head.passed).count();
            format!(
                "{passed}/{} required commands passed",
                validation.commands.len()
            )
        },
    );
    let review = record.review.as_ref().map_or_else(
        || "not recorded".to_string(),
        |review| format!("{} by crew {}", review.verdict.as_str(), review.crew),
    );
    let next = super::next_step(record, &record.binding.task_id);
    runtime.update_task_as_system(
        &record.binding.task_id,
        TaskUpdateParams {
            comment: Some(format!(
                "Review reconciliation {} of merged head {} (pull request {}) settled \
                 {outcome} in run {run_id}. Validation: {validation}. Review: {review}.{} \
                 Next: {next}.",
                record.reconciliation_id,
                pull_request.merged_head.commit,
                pull_request.url,
                record
                    .follow_up_task_id
                    .as_ref()
                    .map(|id| format!(" Follow-up: {id}."))
                    .unwrap_or_default(),
            )),
            ..TaskUpdateParams::default()
        },
        None,
    )?;
    runtime.record_pipeline_audit(
        RECONCILIATION_AUDIT,
        Some(run_id),
        None,
        AuditEventStatus::Success,
        json!({
            "event": "settled",
            "task_id": record.binding.task_id,
            "reconciliation_id": record.reconciliation_id,
            "binding_digest": record.binding_digest,
            "outcome": outcome,
            "follow_up_task_id": record.follow_up_task_id,
        }),
        None,
    )
}

fn update(
    runtime: &OrbitRuntime,
    reconciliation_id: &str,
    change: impl Fn(&mut ReviewReconciliation),
) -> Result<ReviewReconciliation, OrbitError> {
    let workspace_id = runtime.workspace_id()?;
    let store = runtime.review_store()?;
    let mut last_error = None;
    for _ in 0..5 {
        let mut record = store
            .review_reconciliation(&workspace_id, reconciliation_id)?
            .ok_or_else(|| refused(format!("reconciliation {reconciliation_id} not found")))?;
        change(&mut record);
        match store.review_reconciliation_update(&workspace_id, &record) {
            Ok(updated) => return Ok(updated),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.unwrap_or_else(|| OrbitError::Store("reconciliation update failed".into())))
}

fn outcome_view(record: &ReviewReconciliation) -> Value {
    json!({
        "reconciliation_id": record.reconciliation_id,
        "outcome": record.outcome,
        "validation_complete": record.validation.as_ref().is_some_and(|v| v.complete),
        "follow_up_task_id": record.follow_up_task_id,
    })
}
