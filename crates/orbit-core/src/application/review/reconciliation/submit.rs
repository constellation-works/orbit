//! The operator surface: inspect, submit (and replay), status, and the
//! baseline disposition.

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::protocol::tool_input::required_string;
use orbit_common::security::release::sha256_hex;
use orbit_engine::review_gate::{self, RequiredValidationRun};
use orbit_types::task::TaskArtifact;
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::tool::ToolSessionContext;
use orbit_types::workflow::{
    BaselineDisposition, BaselineRemediationCheck, REVIEW_RECONCILIATION_ADMISSION_KEY,
    REVIEW_RECONCILIATION_JOB, REVIEW_RECONCILIATION_SCHEMA_VERSION, ReconciledCommandRun,
    ReconciliationAdmission, ReconciliationAttempt, ReconciliationLog, ReconciliationOutcome,
    ReviewReconciliation,
};
use serde_json::{Value, json};

use super::observe::{contract, observe};
use super::{RECONCILIATION_AUDIT, next_step};
use crate::OrbitRuntime;
use crate::application::job::pipeline::{PipelineSubmission, RetryKey};
use crate::application::task::TaskUpdateParams;

/// Governed tool name; operator-only.
pub(crate) const RECONCILE_REVIEW_OPERATION_ID: &str = "orbit.task.reconcile_review";

/// Run-input field carrying `<reconciliation id>/<attempt>`, the retry key a
/// lost submission response resolves to the run it already admitted.
const ATTEMPT_FIELD: &str = "reconciliation_attempt";
const ATTEMPT_SCAN_LIMIT: usize = 200;

struct Operator {
    actor: String,
    provenance: String,
}

/// `orbit.task.reconcile_review`: `action` is `inspect`, `submit`, `status`
/// or `accept_baseline`. Every action is operator-only; a managed agent can
/// neither produce nor dispose its own evidence.
pub(crate) fn reconcile_review(
    runtime: &OrbitRuntime,
    session_context: &ToolSessionContext,
    input: &Value,
) -> Result<Value, OrbitError> {
    if orbit_common::governance::authorization::agent_context_declared() {
        return Err(OrbitError::CapabilityDenied(
            "managed agents cannot reconcile reviews; an operator must".into(),
        ));
    }
    let authorizer =
        runtime.admit_operator_operation(RECONCILE_REVIEW_OPERATION_ID, session_context)?;
    let operator = Operator {
        actor: authorizer
            .remote_caller_machine_id
            .clone()
            .map_or_else(|| runtime.actor().resolve_write_label(None, None), Ok)?,
        provenance: authorizer.provenance.to_string(),
    };
    let action = required_string(input, &["action"], "action")?;
    let task_id = required_string(input, &["id", "task_id"], "id")?;
    match action.as_str() {
        "inspect" => inspect(runtime, &task_id),
        "status" => status(
            runtime,
            &task_id,
            input.get("reconciliation_id").and_then(Value::as_str),
        ),
        "submit" => {
            runtime.ensure_coordination_task_write_permitted()?;
            let key = required_string(input, &["request_key"], "request_key")?;
            submit(runtime, &operator, &task_id, &key)
        }
        "accept_baseline" => {
            runtime.ensure_coordination_task_write_permitted()?;
            accept_baseline(runtime, &operator, &task_id, input)
        }
        other => Err(OrbitError::InvalidInput(format!(
            "unknown reconcile_review action '{other}'; expected inspect, submit, status or \
             accept_baseline"
        ))),
    }
}

fn inspect(runtime: &OrbitRuntime, task_id: &str) -> Result<Value, OrbitError> {
    let records = records(runtime, task_id)?;
    let observation = match observe(runtime, task_id) {
        Ok(observation) => observation,
        Err(OrbitError::InvalidInput(reason)) => {
            return Ok(json!({
                "id": task_id,
                "eligible": false,
                "refusal": reason,
                "reconciliations": summaries(runtime, task_id, &records)?,
            }));
        }
        Err(error) => return Err(error),
    };
    let contract = contract(runtime, &observation.accepted);
    Ok(json!({
        "id": task_id,
        "eligible": contract.is_ok(),
        "refusal": contract.as_ref().err(),
        "binding": observation.binding,
        "binding_digest": observation.binding_digest,
        "required_commands": contract.as_ref().ok().map(|c| &c.required_commands),
        "review_crew": contract.as_ref().ok().map(|c| &c.review_crew),
        // What a new request key would freeze; an existing record keeps the
        // contract it was submitted under.
        "contract": contract.as_ref().ok(),
        "reconciliations": summaries(runtime, task_id, &records)?,
        "next_step": format!(
            "orbit task reconcile-review submit {task_id} --request <key>"
        ),
    }))
}

fn status(
    runtime: &OrbitRuntime,
    task_id: &str,
    reconciliation_id: Option<&str>,
) -> Result<Value, OrbitError> {
    let mut records = records(runtime, task_id)?;
    if let Some(id) = reconciliation_id {
        records.retain(|record| record.reconciliation_id == id);
        if records.is_empty() {
            return Err(OrbitError::InvalidInput(format!(
                "task {task_id} has no reconciliation {id}"
            )));
        }
    }
    Ok(json!({
        "id": task_id,
        "reconciliations": summaries(runtime, task_id, &records)?,
    }))
}

fn submit(
    runtime: &OrbitRuntime,
    operator: &Operator,
    task_id: &str,
    request_key: &str,
) -> Result<Value, OrbitError> {
    let request_key = request_key.trim();
    if request_key.is_empty()
        || request_key.len() > 128
        || !request_key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
    {
        return Err(OrbitError::InvalidInput(
            "request key must be 1-128 characters of letters, digits, '-', '_', '.' or ':'".into(),
        ));
    }
    let observation = observe(runtime, task_id)?;
    let workspace_id = runtime.workspace_id()?;
    let store = runtime.review_store()?;
    let now = Utc::now();
    let reconciliation_id = format!(
        "rrc-{}",
        &sha256_hex(format!("{workspace_id}\n{task_id}\n{request_key}").as_bytes())[..20]
    );
    // A resubmitted key keeps the contract it was first submitted under; only
    // a new key freezes the owner's current one.
    let contract = match store.review_reconciliation(&workspace_id, &reconciliation_id)? {
        Some(existing) => existing.contract,
        None => contract(runtime, &observation.accepted).map_err(OrbitError::InvalidInput)?,
    };
    let mut record = store.review_reconciliation_open(
        &workspace_id,
        &ReviewReconciliation {
            schema_version: REVIEW_RECONCILIATION_SCHEMA_VERSION,
            reconciliation_id: reconciliation_id.clone(),
            request_key: request_key.to_string(),
            binding: observation.binding,
            binding_digest: observation.binding_digest,
            contract,
            requested_by: operator.actor.clone(),
            requested_at: now,
            attempts: Vec::new(),
            validation: None,
            review: None,
            follow_up_task_id: None,
            outcome: None,
            dispositions: Vec::new(),
            remediation_checks: Vec::new(),
            revision: 0,
            updated_at: now,
        },
    )?;
    if record.runs_settled() || live_run(runtime, &record)?.is_some() {
        audit(runtime, operator, &record, "replayed", None)?;
        return view(runtime, task_id, &record, true);
    }
    // A new attempt runs under the frozen contract; a crew that no longer
    // resolves would only fail it again.
    if let Err(error) = runtime.resolve_crew_for_task(Some(&record.contract.review_crew), None) {
        return Err(OrbitError::InvalidInput(format!(
            "reconciliation {reconciliation_id} reviews on crew '{}', frozen when it was \
             submitted, which cannot be resolved on this host ({error}); restore that crew and \
             resubmit request key `{request_key}`, or submit a new request key to adopt the \
             owner's current configuration",
            record.contract.review_crew
        )));
    }
    let attempt = match record.current_attempt() {
        Some(current) if current.run_id.is_none() => current.attempt,
        Some(current) => current.attempt + 1,
        None => 1,
    };
    if record.current_attempt().map(|current| current.attempt) != Some(attempt) {
        record.attempts.push(ReconciliationAttempt {
            attempt,
            run_id: None,
            admitted_by: operator.actor.clone(),
            admitted_at: now,
        });
        store.review_reconciliation_update(&workspace_id, &record)?;
    }
    let admission = ReconciliationAdmission {
        reconciliation_id: reconciliation_id.clone(),
        attempt,
        authorized_by: operator.actor.clone(),
        authorizer_provenance: operator.provenance.clone(),
        authorized_at: now,
    };
    let input = json!({
        "task_id": task_id,
        "reconciliation_id": reconciliation_id,
        ATTEMPT_FIELD: format!("{reconciliation_id}/{attempt}"),
        REVIEW_RECONCILIATION_ADMISSION_KEY: serde_json::to_value(&admission)
            .map_err(|error| OrbitError::Execution(format!("encode admission: {error}")))?,
    });
    let (submitted, _) = runtime.submit_keyed_pipeline_run(PipelineSubmission {
        reconciliation: true,
        retry_key: Some(RetryKey {
            field: ATTEMPT_FIELD,
            scan_limit: ATTEMPT_SCAN_LIMIT,
        }),
        ..PipelineSubmission::catalog(REVIEW_RECONCILIATION_JOB, input, Some(&operator.actor))
    })?;
    let record = bind_attempt_run(runtime, &reconciliation_id, attempt, &submitted.run_id)?;
    audit(
        runtime,
        operator,
        &record,
        "submitted",
        Some(&submitted.run_id),
    )?;
    view(runtime, task_id, &record, false)
}

/// Record that `attempt` runs as `run_id`. Submission and the run's own
/// prepare step both call this; whichever comes second finds it bound.
pub(super) fn bind_attempt_run(
    runtime: &OrbitRuntime,
    reconciliation_id: &str,
    attempt: u32,
    run_id: &str,
) -> Result<ReviewReconciliation, OrbitError> {
    let workspace_id = runtime.workspace_id()?;
    let store = runtime.review_store()?;
    let mut last_error = None;
    for _ in 0..5 {
        let mut record = store
            .review_reconciliation(&workspace_id, reconciliation_id)?
            .ok_or_else(|| {
                OrbitError::InvalidInput(format!("reconciliation {reconciliation_id} not found"))
            })?;
        let current = record.attempts.last_mut().filter(|a| a.attempt == attempt);
        let Some(current) = current else {
            return Err(OrbitError::InvalidInput(format!(
                "attempt {attempt} is not the current attempt of reconciliation \
                 {reconciliation_id}"
            )));
        };
        match &current.run_id {
            Some(bound) if bound == run_id => return Ok(record),
            Some(bound) => {
                return Err(OrbitError::InvalidInput(format!(
                    "attempt {attempt} of reconciliation {reconciliation_id} already runs as \
                     {bound}"
                )));
            }
            None => current.run_id = Some(run_id.to_string()),
        }
        match store.review_reconciliation_update(&workspace_id, &record) {
            Ok(updated) => return Ok(updated),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.unwrap_or_else(|| OrbitError::Store("reconciliation update failed".into())))
}

/// The current attempt's run while it has not stopped.
fn live_run(
    runtime: &OrbitRuntime,
    record: &ReviewReconciliation,
) -> Result<Option<String>, OrbitError> {
    let Some(run_id) = record
        .current_attempt()
        .and_then(|attempt| attempt.run_id.as_deref())
    else {
        return Ok(None);
    };
    Ok(runtime
        .get_job_run_backend(run_id)?
        .filter(|run| !run.state.is_terminal())
        .map(|_| run_id.to_string()))
}

fn accept_baseline(
    runtime: &OrbitRuntime,
    operator: &Operator,
    task_id: &str,
    input: &Value,
) -> Result<Value, OrbitError> {
    let reconciliation_id = required_string(input, &["reconciliation_id"], "reconciliation_id")?;
    let command = required_string(input, &["command"], "command")?;
    let remediation = required_string(input, &["remediation_commit"], "remediation_commit")?;
    let reason = required_string(input, &["reason"], "reason")?;
    let refused = |message: String| OrbitError::InvalidInput(message);
    let workspace_id = runtime.workspace_id()?;
    let store = runtime.review_store()?;
    let mut record = store
        .review_reconciliation(&workspace_id, &reconciliation_id)?
        .filter(|record| record.binding.task_id == task_id)
        .ok_or_else(|| {
            refused(format!(
                "task {task_id} has no reconciliation {reconciliation_id}"
            ))
        })?;
    let repo = &runtime.paths().repo_root;
    let remediation_commit = review_gate::fetch_landed_commit(repo, &remediation)
        .and_then(|()| review_gate::revision(repo, &remediation))
        .map_err(|error| {
            refused(format!(
                "remediation {remediation} is not readable in the owner checkout ({error})"
            ))
        })?
        .commit;
    if let Some(existing) = record
        .dispositions
        .iter()
        .find(|disposition| disposition.command == command)
    {
        if existing.remediation_commit == remediation_commit {
            return view(runtime, task_id, &record, true);
        }
        return Err(refused(format!(
            "`{command}` was already disposed with remediation {}",
            existing.remediation_commit
        )));
    }
    let Some(ReconciliationOutcome::AwaitingDisposition { commands }) = &record.outcome else {
        return Err(refused(format!(
            "reconciliation {reconciliation_id} is not waiting for a baseline disposition"
        )));
    };
    let commands = commands.clone();
    if !commands.contains(&command) {
        return Err(refused(format!(
            "`{command}` is not a baseline failure of reconciliation {reconciliation_id}; \
             waiting on: {}",
            commands.join(", ")
        )));
    }
    let entry = record
        .validation
        .as_ref()
        .and_then(|validation| validation.commands.iter().find(|c| c.command == command))
        .filter(|entry| entry.reproduced_on_base())
        .cloned()
        .ok_or_else(|| {
            refused(format!(
                "`{command}` has no recorded failure reproduced at the base"
            ))
        })?;
    let baseline = entry
        .baseline
        .as_ref()
        .ok_or_else(|| refused(format!("`{command}` has no baseline run")))?;
    let binding_digest = record.binding_digest.clone();
    let head = record.binding.pull_request.merged_head.commit.clone();
    let landing = record.binding.pull_request.landing_branch.clone();
    let landed = [format!("origin/{landing}"), landing.clone()]
        .iter()
        .any(|tip| review_gate::contains_commit(repo, &remediation_commit, tip).unwrap_or(false));
    if !landed {
        return Err(refused(format!(
            "remediation {remediation_commit} has not landed on {landing}; land the fix first"
        )));
    }
    if review_gate::contains_commit(repo, &remediation_commit, &head)? {
        return Err(refused(format!(
            "remediation {remediation_commit} is already part of the merged head {head}, so it \
             cannot explain a failure of that head"
        )));
    }
    let existing_check = record
        .remediation_checks
        .iter()
        .find(|check| {
            check.command == command
                && check.head_commit == head
                && check.remediation_commit == remediation_commit
        })
        .cloned();
    let check = match existing_check {
        Some(check) if check.run.passed => check.clone(),
        Some(check) => {
            return Err(refused(format!(
                "`{command}` failed at remediation {remediation_commit}; no disposition was \
                 recorded (see {})",
                check.run.log.path
            )));
        }
        None => {
            let run = run_remediation_command(
                runtime,
                &record,
                &command,
                &head,
                &remediation_commit,
                operator,
            )?;
            let check = BaselineRemediationCheck {
                command: command.clone(),
                head_commit: head.clone(),
                remediation_commit: remediation_commit.clone(),
                run,
                actor: operator.actor.clone(),
                provenance: operator.provenance.clone(),
                checked_at: Utc::now(),
            };
            record = record_remediation_check(runtime, &record, &check)?;
            let check = record
                .remediation_checks
                .iter()
                .find(|recorded| {
                    recorded.command == command
                        && recorded.head_commit == head
                        && recorded.remediation_commit == remediation_commit
                })
                .cloned()
                .ok_or_else(|| refused("remediation check was not retained".into()))?;
            if !check.run.passed {
                return Err(refused(format!(
                    "`{command}` did not pass at remediation {remediation_commit}; no \
                     disposition was recorded (see {})",
                    check.run.log.path
                )));
            }
            check
        }
    };
    // A long remediation command must not authorize a decision after the
    // provider's head, task meaning, or baseline moved while it ran.
    let remediation_still_landed = [format!("origin/{landing}"), landing.clone()]
        .iter()
        .any(|tip| review_gate::contains_commit(repo, &remediation_commit, tip).unwrap_or(false));
    if !remediation_still_landed {
        return Err(refused(format!(
            "remediation {remediation_commit} is no longer present on {landing}; fetch the \
             landing branch and retry"
        )));
    }
    let current = observe(runtime, task_id)?;
    if current.binding_digest != binding_digest {
        return Err(refused(format!(
            "the delivery changed since reconciliation {reconciliation_id} observed it; submit a \
             new request key"
        )));
    }
    record.dispositions.push(BaselineDisposition {
        command: command.clone(),
        head_commit: head.clone(),
        failure_log_sha256: entry.head.log.sha256.clone(),
        baseline_log_sha256: baseline.log.sha256.clone(),
        remediation_commit: remediation_commit.clone(),
        remediation_check: Some(check.run.log.clone()),
        reason: reason.trim().to_string(),
        actor: operator.actor.clone(),
        provenance: operator.provenance.clone(),
        recorded_at: Utc::now(),
    });
    let disposed = commands
        .iter()
        .all(|command| record.dispositions.iter().any(|d| &d.command == command));
    if disposed {
        record.outcome = Some(ReconciliationOutcome::AcceptedWithDisposition);
    }
    let record = store.review_reconciliation_update(&workspace_id, &record)?;
    runtime.update_task_as_system(
        task_id,
        TaskUpdateParams {
            comment: Some(format!(
                "Review reconciliation {reconciliation_id}: {} accepted the baseline failure of \
                 `{command}` at merged head {head}, remediated by landed commit \
                 {remediation_commit} ({}). Validation of the merged head stays incomplete.{}",
                operator.actor,
                reason.trim(),
                if disposed {
                    " Every baseline failure is disposed; the task can complete from review."
                } else {
                    ""
                }
            )),
            ..TaskUpdateParams::default()
        },
        None,
    )?;
    audit(runtime, operator, &record, "baseline_disposed", None)?;
    view(runtime, task_id, &record, false)
}

fn remediation_check_path(
    record: &ReviewReconciliation,
    command: &str,
    remediation_commit: &str,
    nonce: u128,
) -> String {
    let key = sha256_hex(
        format!(
            "{}\n{}\n{}",
            record.reconciliation_id, command, remediation_commit
        )
        .as_bytes(),
    );
    format!(
        "review-reconciliation/{}/remediation-{}-{nonce}.json",
        record.reconciliation_id,
        &key[..20]
    )
}

fn run_remediation_command(
    runtime: &OrbitRuntime,
    record: &ReviewReconciliation,
    command: &str,
    head: &str,
    remediation_commit: &str,
    operator: &Operator,
) -> Result<ReconciledCommandRun, OrbitError> {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| OrbitError::Execution(format!("read system clock: {error}")))?
        .as_nanos();
    let path = remediation_check_path(record, command, remediation_commit, nonce);
    let key = sha256_hex(path.as_bytes());
    let checkout_id = format!(
        "jrun-reconcile-{}-{}-{nonce}",
        &key[..16],
        std::process::id()
    );
    let checkout = runtime.create_recovery_checkout(&checkout_id, remediation_commit)?;
    let run = review_gate::run_required_validation(runtime, &checkout, command);
    let cleanup = runtime.remove_recovery_checkout(&checkout_id);
    let run: RequiredValidationRun = run?;
    cleanup?;
    let value = json!({
        "schema_version": 1,
        "reconciliation_id": record.reconciliation_id,
        "task_id": record.binding.task_id,
        "command": command,
        "tested_head": head,
        "tested_commit": remediation_commit,
        "passed": run.passed,
        "exit_code": run.exit_code,
        "timed_out": run.timed_out,
        "failure_kind": run.failure_kind,
        "validation_env": run.environment,
        "actor": operator.actor,
        "provenance": operator.provenance,
        "output": run.output,
    });
    let content = serde_json::to_vec_pretty(&value)
        .map_err(|error| OrbitError::Execution(format!("encode {path}: {error}")))?;
    let log = ReconciliationLog {
        path: path.clone(),
        sha256: sha256_hex(&content),
    };
    runtime.update_task_as_system(
        &record.binding.task_id,
        TaskUpdateParams {
            upsert_artifacts: vec![TaskArtifact {
                path,
                content,
                media_type: "application/json".into(),
                created_by: None,
            }],
            ..TaskUpdateParams::default()
        },
        None,
    )?;
    Ok(ReconciledCommandRun {
        commit: remediation_commit.to_string(),
        passed: run.passed,
        exit_code: Some(run.exit_code),
        failure_kind: run.failure_kind,
        log,
    })
}

fn record_remediation_check(
    runtime: &OrbitRuntime,
    record: &ReviewReconciliation,
    check: &BaselineRemediationCheck,
) -> Result<ReviewReconciliation, OrbitError> {
    let workspace_id = runtime.workspace_id()?;
    let store = runtime.review_store()?;
    let mut last_error = None;
    for _ in 0..5 {
        let mut current = store
            .review_reconciliation(&workspace_id, &record.reconciliation_id)?
            .ok_or_else(|| {
                OrbitError::InvalidInput(format!(
                    "review reconciliation '{}' does not exist",
                    record.reconciliation_id
                ))
            })?;
        if let Some(existing) = current.remediation_checks.iter().find(|existing| {
            existing.command == check.command
                && existing.head_commit == check.head_commit
                && existing.remediation_commit == check.remediation_commit
        }) {
            if existing.run.log != check.run.log || existing.run.passed != check.run.passed {
                return Ok(current);
            }
            return Ok(current);
        }
        current.remediation_checks.push(check.clone());
        match store.review_reconciliation_update(&workspace_id, &current) {
            Ok(updated) => return Ok(updated),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.unwrap_or_else(|| OrbitError::Store("remediation check update failed".into())))
}

fn records(runtime: &OrbitRuntime, task_id: &str) -> Result<Vec<ReviewReconciliation>, OrbitError> {
    runtime.get_task(task_id)?;
    runtime
        .review_store()?
        .review_reconciliations_for_task(&runtime.workspace_id()?, task_id)
}

fn summaries(
    runtime: &OrbitRuntime,
    task_id: &str,
    records: &[ReviewReconciliation],
) -> Result<Vec<Value>, OrbitError> {
    records
        .iter()
        .map(|record| summary(runtime, task_id, record))
        .collect()
}

fn summary(
    runtime: &OrbitRuntime,
    task_id: &str,
    record: &ReviewReconciliation,
) -> Result<Value, OrbitError> {
    let run_id = record
        .current_attempt()
        .and_then(|attempt| attempt.run_id.clone());
    let run_state = match &run_id {
        Some(run_id) => runtime
            .get_job_run_backend(run_id)?
            .map(|run| run.state.to_string()),
        None => None,
    };
    Ok(json!({
        "reconciliation_id": record.reconciliation_id,
        "outcome": record.outcome.as_ref().map(ReconciliationOutcome::as_str),
        "run_id": run_id,
        "run_state": run_state,
        "next_step": next_step(record, task_id),
        "record": record,
    }))
}

fn view(
    runtime: &OrbitRuntime,
    task_id: &str,
    record: &ReviewReconciliation,
    replayed: bool,
) -> Result<Value, OrbitError> {
    let mut value = summary(runtime, task_id, record)?;
    value["id"] = json!(task_id);
    value["replayed"] = json!(replayed);
    Ok(value)
}

fn audit(
    runtime: &OrbitRuntime,
    operator: &Operator,
    record: &ReviewReconciliation,
    event: &str,
    run_id: Option<&str>,
) -> Result<(), OrbitError> {
    runtime.record_pipeline_audit(
        RECONCILIATION_AUDIT,
        run_id,
        Some(&operator.actor),
        AuditEventStatus::Success,
        json!({
            "event": event,
            "task_id": record.binding.task_id,
            "reconciliation_id": record.reconciliation_id,
            "binding_digest": record.binding_digest,
            "authorizer_provenance": operator.provenance,
            "outcome": record.outcome.as_ref().map(ReconciliationOutcome::as_str),
        }),
        None,
    )
}
