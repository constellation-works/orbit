//! Bind and validate completion results against owner evidence.

use chrono::{DateTime, Utc};
use orbit_automation::condition::CheckedAutoTaskStep;
use orbit_common::OrbitError;
use orbit_engine::AutoTaskObservation;
use orbit_types::task::Task;
use orbit_types::workflow::{
    AUTO_TASK_EVIDENCE_SCHEMA_VERSION, AutoTaskCompletionContract, AutoTaskCompletionReceipt,
    AutoTaskDefinition, AutoTaskEvidenceBinding, AutoTaskResultStatus, AutoTaskStepRecord,
    AutoTaskStepStatus,
};

use super::super::step_observation::StepContext;
use super::{adapter, contract, trust};
use crate::OrbitRuntime;

/// Judge the bound task's completion and put the outcome on the observation:
/// an accepted receipt carries the typed result, anything else holds the step.
/// Runs the adapter, so only the engine's own pass calls this; the admission
/// and start guards share the read-only observation.
pub(in crate::application::auto_tasks) fn bind_completion_result(
    runtime: &OrbitRuntime,
    step: &StepContext,
    observation: &mut AutoTaskObservation,
    now: DateTime<Utc>,
) {
    let Some(record) = step.record.as_ref().filter(|r| !r.status.is_settled()) else {
        return;
    };
    let Some(receipt) = observation.receipt.as_mut().filter(|r| r.accepted) else {
        return;
    };
    match bound_completion_outcome(runtime, step, record, &receipt.task_id, now) {
        BoundCompletion::Ordinary => {}
        BoundCompletion::Held(hold) => observation.result_hold = Some(hold),
        // Recovery in time clears a hold; recovery after the deadline does
        // not, because the expiry already decided.
        BoundCompletion::Accepted(_)
            if record.status == AutoTaskStepStatus::Held && observation.expired.is_some() =>
        {
            observation.result_hold = Some("its result was held at expiry".to_string());
        }
        BoundCompletion::Accepted(result) => receipt.result = Some(*result),
    }
}

enum BoundCompletion {
    /// Neither the step nor the definition declares a contract.
    Ordinary,
    Accepted(Box<AutoTaskCompletionReceipt>),
    Held(String),
}

fn bound_completion_outcome(
    runtime: &OrbitRuntime,
    step: &StepContext,
    record: &AutoTaskStepRecord,
    task_id: &str,
    now: DateTime<Utc>,
) -> BoundCompletion {
    let name = &step.checked.auto_task;
    let required = step.checked.require_result.as_deref();
    // A definition gone since the mint leaves a step that requires no result
    // ordinary: only a step that asked for proof can be short of it.
    let definition = match (step.minting_definition(runtime, now), required) {
        (Ok(Some(definition)), _) => definition,
        (_, None) => return BoundCompletion::Ordinary,
        (Ok(None), Some(_)) => {
            return BoundCompletion::Held(format!("auto-task `{name}` did not resolve"));
        }
        (Err(error), Some(_)) => {
            return BoundCompletion::Held(format!("auto-task `{name}`: {error}"));
        }
    };
    let contract = match contract::declared_contract(name, required, &definition) {
        Ok(Some(contract)) => contract,
        Ok(None) => return BoundCompletion::Ordinary,
        Err(hold) => return BoundCompletion::Held(hold),
    };
    let task = match runtime.get_task(task_id) {
        Ok(task) => task,
        Err(error) => {
            return BoundCompletion::Held(format!("task {task_id} is unreadable: {error}"));
        }
    };
    match validate_completion(runtime, step, &step.checked, record, &contract, &task, now) {
        Ok(result) => BoundCompletion::Accepted(Box::new(result)),
        Err(hold) => BoundCompletion::Held(hold),
    }
}

/// Validate one completed task against owner records, then its adapter.
pub(super) fn validate_completion(
    runtime: &OrbitRuntime,
    step: &StepContext,
    checked: &CheckedAutoTaskStep,
    record: &AutoTaskStepRecord,
    contract: &AutoTaskCompletionContract,
    task: &Task,
    now: DateTime<Utc>,
) -> Result<AutoTaskCompletionReceipt, String> {
    let trusted = trust::trusted_identity(runtime, step, checked, record, task)?;
    let completed_at = trusted.completed_at;
    let (candidate, evidence, artifacts) = trust::admitted_evidence(runtime, contract, &trusted)?;
    let (verdict, adapter_digest) = adapter::run_adapter(
        runtime,
        step,
        checked,
        contract,
        &trusted,
        (&candidate, &artifacts),
        task,
    )?;
    if verdict.status != AutoTaskResultStatus::Passed {
        let reasons = verdict
            .reasons
            .iter()
            .map(|reason| format!("{}: {}", reason.code, reason.detail))
            .collect::<Vec<_>>()
            .join("; ");
        return Err(format!(
            "`{}` result is {} for task {}: {}",
            contract.contract,
            verdict.status.as_str(),
            task.id,
            if reasons.is_empty() {
                "the adapter gave no reason"
            } else {
                &reasons
            }
        ));
    }
    Ok(AutoTaskCompletionReceipt {
        contract: contract.contract.clone(),
        status: AutoTaskResultStatus::Passed,
        workspace_id: trusted.binding.workspace_id,
        definition: trusted.binding.definition,
        definition_digest: trusted.binding.definition_digest,
        task_id: trusted.binding.task_id,
        run_id: trusted.binding.run_id,
        revision: trusted.binding.source_revision,
        completed_at,
        evidence,
        adapter: contract.adapter.clone(),
        adapter_digest,
        accepted_at: now,
    })
}

pub(super) fn binding_mismatch(
    claimed: &AutoTaskEvidenceBinding,
    trusted: &AutoTaskEvidenceBinding,
) -> Option<&'static str> {
    [
        (claimed.workspace_id == trusted.workspace_id, "workspace"),
        (claimed.definition == trusted.definition, "definition"),
        (
            claimed.definition_digest == trusted.definition_digest,
            "definition digest",
        ),
        (claimed.task_id == trusted.task_id, "task"),
        (claimed.run_id == trusted.run_id, "run"),
        (
            claimed.source_revision == trusted.source_revision,
            "source revision",
        ),
    ]
    .into_iter()
    .find_map(|(matches, field)| (!matches).then_some(field))
}

/// The instructions a contract adds to a task it mints: what the owner will
/// compare the evidence with. The task and run ids are the worker's own.
pub(in crate::application::auto_tasks) fn evidence_binding_note(
    runtime: &OrbitRuntime,
    step: &StepContext,
    definition: &AutoTaskDefinition,
) -> Result<Option<String>, OrbitError> {
    let Some(contract) = &definition.completion else {
        return Ok(None);
    };
    let workspace_id = runtime.workspace_id()?;
    let digest = step
        .checkpoint
        .frozen_targets
        .get(&definition.name)
        .map_or("", |frozen| frozen.digest.as_str());
    let revision = step.checkpoint.source_revision.as_deref().unwrap_or("");
    let instructions = contract
        .evidence_instructions
        .as_deref()
        .map_or(String::new(), |text| format!("\n\n{}", text.trim()));
    Ok(Some(format!(
        "\n\n## Required evidence\n\nThis task completes a job step that requires `{contract}`. \
         Done alone does not satisfy it. Before finishing, attach every evidence file with \
         `orbit.task.artifact.put`, then attach `{artifact}` as JSON with exactly these fields, \
         adding no time or status:\n\n```json\n{{\n  \"schema_version\": {version},\n  \
         \"contract\": \"{contract}\",\n  \"binding\": {{\n    \"workspace_id\": \
         \"{workspace_id}\",\n    \"definition\": \"{name}\",\n    \"definition_digest\": \
         \"{digest}\",\n    \"task_id\": \"<this task>\",\n    \"run_id\": \"<this run>\",\n    \
         \"source_revision\": \"{revision}\"\n  }},\n  \"artifacts\": [{{\"path\": \"<file>\", \
         \"sha256\": \"<sha256>\"}}],\n  \"task_refs\": [\"<tasks the evidence cites>\"]\n}}\n```\n\n\
         Work from exactly the source revision above. Evidence for any other revision, or written \
         after this task is done, is not accepted.{instructions}",
        contract = contract.contract,
        artifact = contract.evidence_artifact,
        version = AUTO_TASK_EVIDENCE_SCHEMA_VERSION,
        name = definition.name,
    )))
}
