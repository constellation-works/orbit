//! Completion contracts of auto-task job steps
//! (`docs/design/activity-job/specs/auto-task-step.md` §4): resolve a step's
//! declared contract, judge a completed task's evidence against the owner's
//! records and a granted workspace adapter, and keep the accepted-result
//! history.
//!
//! A receipt's task, run, digest, source and time come from durable owner
//! records. The evidence a run leaves is compared with them, and an adapter's
//! verdict only says whether the evidence meets the workspace's policy: neither
//! is a certificate, and an agent's final message is never read. What a form
//! requires lives in the workspace adapter, so nothing here knows a form.
//!
//! Every failure holds the step. A hold is rechecked on later passes until the
//! step's expiry, so a finding resolved in time can clear it, while a hold still
//! standing at expiry times out.

use std::collections::BTreeSet;
use std::io::Read;
use std::path::PathBuf;

use chrono::{DateTime, Duration, Utc};
use orbit_automation::condition::CheckedAutoTaskStep;
use orbit_common::OrbitError;
use orbit_common::fs::io::{atomic_write_text, with_exclusive_file_lock};
use orbit_common::fs::open_read_only_no_follow;
use orbit_common::security::release::sha256_hex;
use orbit_engine::activity_job::script_stdout_output;
use orbit_engine::{AutoTaskObservation, ScriptStepEnd, ScriptStepRequest, ScriptStepRun};
use orbit_types::task::{ArtifactManifestFileV2, Task, TaskHistoryEntry, TaskStatus};
use orbit_types::workflow::{
    AUTO_TASK_ACCEPTED_HISTORY_SCHEMA_VERSION, AUTO_TASK_EVIDENCE_SCHEMA_VERSION,
    AutoTaskAcceptedHistory, AutoTaskAcceptedHistoryFile, AutoTaskAdapterVerdict,
    AutoTaskCompletionContract, AutoTaskCompletionReceipt, AutoTaskDefinition,
    AutoTaskEvidenceBinding, AutoTaskEvidenceCandidate, AutoTaskEvidenceRef, AutoTaskResultStatus,
    AutoTaskStepRecord, AutoTaskStepStatus,
};
use serde_json::{Map, Value, json};

use super::step_observation::{StepContext, step_input_digest};
use crate::OrbitRuntime;
use crate::application::automation::source::Source;
use crate::application::job::scripts::step::run_script_step;
use crate::runtime::run_input::carried_task_ids;
use crate::runtime::scripts::ScriptGrantState;

/// Where the latest accepted receipt per definition is kept. Host-local, like
/// the schedule cursor beside it: the owner is the one host that accepts.
fn accepted_history_path(runtime: &OrbitRuntime) -> PathBuf {
    runtime
        .paths()
        .state_dir
        .join("auto-task-accepted-results.json")
}

/// How far a recorded proof is rechecked. Reading records is cheap enough for
/// every pass; asking the adapter again is reserved for the admission fence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ProofDepth {
    Records,
    Adapter,
}

/// The contract this step is held to: `None` for an ordinary step on a
/// definition that declares none. A contract on only one side, or two
/// different ones, can never produce a result, so the step holds.
fn declared_contract(
    step_name: &str,
    required: Option<&str>,
    definition: &AutoTaskDefinition,
) -> Result<Option<AutoTaskCompletionContract>, String> {
    match (required, &definition.completion) {
        (None, None) => Ok(None),
        (Some(required), None) => Err(format!(
            "require_result `{required}`: auto-task `{step_name}` declares no completion \
             contract, so its result would be unavailable; nothing is minted"
        )),
        (None, Some(declared)) => Err(format!(
            "auto-task `{step_name}` declares the completion contract `{}`, so this step must \
             set require_result to it; nothing is minted",
            declared.contract
        )),
        (Some(required), Some(declared)) if declared.contract != required => Err(format!(
            "require_result `{required}`: auto-task `{step_name}` declares `{}` instead; nothing \
             is minted",
            declared.contract
        )),
        (Some(_), Some(declared)) => Ok(Some(declared.clone())),
    }
}

/// Why a step's contract cannot hold a result now: its contract does not
/// resolve, its adapter is not granted, the pinned source moved, or an earlier
/// proof it relies on no longer stands. `None` for an ordinary step.
pub(super) fn contract_admission_hold(
    runtime: &OrbitRuntime,
    step: &StepContext,
    now: DateTime<Utc>,
) -> Option<String> {
    let name = &step.checked.auto_task;
    let definition = match step.minting_definition(runtime, now) {
        Ok(Some(definition)) => definition,
        Ok(None) => return Some(format!("auto-task `{name}` did not resolve")),
        Err(error) => return Some(format!("auto-task `{name}`: {error}")),
    };
    let contract =
        match declared_contract(name, step.checked.require_result.as_deref(), &definition) {
            Ok(Some(contract)) => contract,
            Ok(None) => return None,
            Err(hold) => return Some(hold),
        };
    adapter_grant_hold(runtime, &contract)
        .or_else(|| source_freshness_hold(runtime, step))
        .or_else(|| prior_proof_hold(runtime, step, now, ProofDepth::Records))
}

/// A read-only check that the contract's adapter would run. Absent or
/// ungranted means unavailable: nothing is executed or granted to find out.
fn adapter_grant_hold(
    runtime: &OrbitRuntime,
    contract: &AutoTaskCompletionContract,
) -> Option<String> {
    let adapter = &contract.adapter;
    let unavailable = |why: String| {
        Some(format!(
            "`{}` evidence is unavailable: adapter script `{adapter}` {why}",
            contract.contract
        ))
    };
    match runtime.script_status(adapter) {
        Ok(Some(status)) if status.state == ScriptGrantState::Granted => None,
        Ok(Some(status)) => unavailable(format!(
            "is {}; an operator reviews and grants it",
            status.state.as_str()
        )),
        Ok(None) => unavailable("is not installed in this workspace".to_string()),
        Err(error) => unavailable(format!("is unreadable: {error}")),
    }
}

/// Whether the integration branch still sits at the revision this execution
/// pinned. A branch advance means evidence for the pin no longer describes it.
fn source_freshness_hold(runtime: &OrbitRuntime, step: &StepContext) -> Option<String> {
    let Some(pinned) = step.checkpoint.source_revision.as_deref() else {
        return Some("the execution pinned no source revision".to_string());
    };
    let branch = runtime.workflow_base_branch().to_string();
    match Source::new(&runtime.paths().repo_root).verify_branch(&branch) {
        Ok(tip) if tip.commit == pinned => None,
        Ok(tip) => Some(format!(
            "the pinned source is stale: `{branch}` moved from {pinned} to {}; start a fresh run",
            tip.commit
        )),
        Err(error) => Some(format!(
            "the pinned source is unavailable: `{branch}` did not resolve: {error}"
        )),
    }
}

/// Why a step may not rely on the contract steps already settled in this
/// execution. A reused checkpoint is rechecked exactly like a fresh one, and a
/// skipped or otherwise unaccepted contract step is not evidence. A branch of
/// this step's own parallel block is left to its existing join rule.
pub(super) fn prior_proof_hold(
    runtime: &OrbitRuntime,
    step: &StepContext,
    now: DateTime<Utc>,
    depth: ProofDepth,
) -> Option<String> {
    for other in &step.steps {
        if other.id == step.checked.id
            || other.require_result.is_none()
            || (other.parallel.is_some() && other.parallel == step.checked.parallel)
        {
            continue;
        }
        let status = step.step_facts(&other.id).status;
        let reason = match status.as_str() {
            "done" => proof_stands(runtime, step, other, now, depth).err(),
            "skipped" | "failed" | "cancelled" | "timeout" => Some(format!(
                "it ended {status}, which is not an accepted result"
            )),
            _ => None,
        };
        if let Some(reason) = reason {
            return Some(format!("required proof from step `{}`: {reason}", other.id));
        }
    }
    None
}

fn proof_stands(
    runtime: &OrbitRuntime,
    step: &StepContext,
    other: &CheckedAutoTaskStep,
    now: DateTime<Utc>,
    depth: ProofDepth,
) -> Result<(), String> {
    let record = step
        .checkpoint
        .steps
        .get(&other.id)
        .ok_or("it succeeded without an auto-task record, so it has no receipt")?;
    let result = record
        .receipt
        .as_ref()
        .and_then(|receipt| receipt.result.as_ref())
        .ok_or("it has no validated result; a raw successful step is not evidence")?;
    let (contract, frozen_digest) = frozen_contract(step, other)?;
    if result.status != AutoTaskResultStatus::Passed {
        return Err(format!("its result is {}", result.status.as_str()));
    }
    if record.task_id.as_deref() != Some(result.task_id.as_str()) {
        return Err("its result names a different task".to_string());
    }
    if other.require_result.as_deref() != Some(result.contract.as_str()) {
        return Err(format!("its result is for contract `{}`", result.contract));
    }
    if result.definition_digest != frozen_digest {
        return Err("its result is for a different definition digest".to_string());
    }
    if step.checkpoint.source_revision.as_deref() != Some(result.revision.as_str()) {
        return Err(format!(
            "its result covers revision {}, not the pinned one",
            result.revision
        ));
    }
    let age = now - result.completed_at;
    if age > Duration::minutes(i64::from(contract.max_age_minutes)) {
        return Err(format!(
            "its result from {} is older than the {} minute limit; start a fresh run",
            result.completed_at.to_rfc3339(),
            contract.max_age_minutes
        ));
    }
    if age < Duration::zero() {
        return Err("its result is dated in the future".to_string());
    }
    let task = runtime
        .get_task(&result.task_id)
        .map_err(|error| format!("task {} is unreadable: {error}", result.task_id))?;
    let history = runtime
        .get_task_history(&result.task_id)
        .map_err(|error| format!("task {} history is unreadable: {error}", result.task_id))?;
    if task.status != TaskStatus::Done
        || trusted_completion_time(&history).as_ref() != Some(&result.completed_at)
    {
        return Err(format!(
            "task {} is no longer the completion it recorded",
            result.task_id
        ));
    }
    let manifest = runtime
        .get_task_artifact_manifest(&result.task_id)
        .map_err(|error| format!("its evidence is unreadable: {error}"))?;
    for evidence in &result.evidence {
        if !manifest
            .iter()
            .any(|file| file.path == evidence.path && file.sha256 == evidence.sha256)
        {
            return Err(format!("its evidence `{}` changed", evidence.path));
        }
    }
    if depth == ProofDepth::Adapter {
        validate_completion(runtime, step, other, record, &contract, &task, now)?;
    }
    Ok(())
}

fn frozen_contract(
    step: &StepContext,
    other: &CheckedAutoTaskStep,
) -> Result<(AutoTaskCompletionContract, String), String> {
    let frozen = step
        .checkpoint
        .frozen_targets
        .get(&other.auto_task)
        .ok_or_else(|| format!("auto-task `{}` was not frozen", other.auto_task))?;
    let definition: AutoTaskDefinition = serde_json::from_str(&frozen.source)
        .map_err(|error| format!("frozen auto-task `{}`: {error}", other.auto_task))?;
    let contract = declared_contract(
        &other.auto_task,
        other.require_result.as_deref(),
        &definition,
    )?
    .ok_or_else(|| format!("auto-task `{}` declares no contract", other.auto_task))?;
    Ok((contract, frozen.digest.clone()))
}

/// Revalidate a historical accepted result that a step's `reuse_when` matched,
/// as if this execution had bound its task: the same contract and definition
/// digest, this execution's pinned revision, the contract's maximum age, a
/// creation receipt minted for the same inputs, unchanged completion and
/// evidence, and the adapter's verdict now. Returns the receipt the step
/// settles on, or why the result does not apply, in which case the step
/// mints fresh evidence.
pub(super) fn reused_result(
    runtime: &OrbitRuntime,
    step: &StepContext,
    record: &AutoTaskStepRecord,
    candidate: &AutoTaskCompletionReceipt,
    now: DateTime<Utc>,
) -> Result<AutoTaskCompletionReceipt, String> {
    let name = &step.checked.auto_task;
    let definition = step
        .minting_definition(runtime, now)
        .map_err(|error| format!("auto-task `{name}`: {error}"))?
        .ok_or_else(|| format!("auto-task `{name}` did not resolve"))?;
    let contract = declared_contract(name, step.checked.require_result.as_deref(), &definition)?
        .ok_or("the step declares no completion contract, so no accepted result applies")?;
    let frozen = step
        .checkpoint
        .frozen_targets
        .get(name)
        .ok_or_else(|| format!("auto-task `{name}` was not frozen"))?;
    if candidate.definition != *name || candidate.status != AutoTaskResultStatus::Passed {
        return Err(format!(
            "the accepted result is a {} result for `{}`",
            candidate.status.as_str(),
            candidate.definition
        ));
    }
    if candidate.contract != contract.contract {
        return Err(format!(
            "the accepted result is for contract `{}`",
            candidate.contract
        ));
    }
    if candidate.definition_digest != frozen.digest {
        return Err("the accepted result is for a different definition digest".to_string());
    }
    if step.checkpoint.source_revision.as_deref() != Some(candidate.revision.as_str()) {
        return Err(format!(
            "the accepted result covers revision {}, not the pinned one",
            candidate.revision
        ));
    }
    let age = now - candidate.completed_at;
    if age > Duration::minutes(i64::from(contract.max_age_minutes)) {
        return Err(format!(
            "the accepted result from {} is older than the {} minute limit",
            candidate.completed_at.to_rfc3339(),
            contract.max_age_minutes
        ));
    }
    if age < Duration::zero() {
        return Err("the accepted result is dated in the future".to_string());
    }
    let task = runtime
        .get_task(&candidate.task_id)
        .map_err(|error| format!("task {} is unreadable: {error}", candidate.task_id))?;
    let revalidated =
        validate_completion(runtime, step, &step.checked, record, &contract, &task, now)?;
    if revalidated.completed_at != candidate.completed_at
        || revalidated.run_id != candidate.run_id
        || revalidated.evidence != candidate.evidence
    {
        return Err(format!(
            "task {} is no longer the completion its accepted result recorded",
            candidate.task_id
        ));
    }
    Ok(revalidated)
}

/// Judge the bound task's completion and put the outcome on the observation:
/// an accepted receipt carries the typed result, anything else holds the step.
/// Runs the adapter, so only the engine's own pass calls this; the admission
/// and start guards share the read-only observation.
pub(super) fn bind_completion_result(
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
    let contract = match declared_contract(name, required, &definition) {
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
fn validate_completion(
    runtime: &OrbitRuntime,
    step: &StepContext,
    checked: &CheckedAutoTaskStep,
    record: &AutoTaskStepRecord,
    contract: &AutoTaskCompletionContract,
    task: &Task,
    now: DateTime<Utc>,
) -> Result<AutoTaskCompletionReceipt, String> {
    let trusted = trusted_identity(runtime, step, checked, record, task)?;
    let completed_at = trusted.completed_at;
    let (candidate, evidence, artifacts) = admitted_evidence(runtime, contract, &trusted)?;
    let (verdict, adapter_digest) = run_adapter(
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

/// What the owner's records say this completion is.
struct TrustedIdentity {
    binding: AutoTaskEvidenceBinding,
    completed_at: DateTime<Utc>,
    /// The machine the claim record names, when the run lives on another host.
    execution_machine: Option<String>,
}

fn trusted_identity(
    runtime: &OrbitRuntime,
    step: &StepContext,
    checked: &CheckedAutoTaskStep,
    record: &AutoTaskStepRecord,
    task: &Task,
) -> Result<TrustedIdentity, String> {
    if task.status != TaskStatus::Done {
        return Err(format!("task {} is not done", task.id));
    }
    let source_revision = step
        .checkpoint
        .source_revision
        .clone()
        .ok_or("the execution pinned no source revision")?;
    let frozen = step
        .checkpoint
        .frozen_targets
        .get(&checked.auto_task)
        .ok_or_else(|| format!("auto-task `{}` was not frozen", checked.auto_task))?;
    let history = runtime
        .get_task_history(&task.id)
        .map_err(|error| format!("task {} history is unreadable: {error}", task.id))?;
    // The creation receipt, not the agent-writable tag, ties the task to this
    // definition and source. Only the minting step owns that reservation key;
    // a compatible consumer binds the same task with its own independent key.
    let invocation = super::job_step::creation_receipt(&history).ok_or_else(|| {
        format!(
            "task {} has no creation receipt for this execution",
            task.id
        )
    })?;
    let expected_digest = step_input_digest(
        &checked.auto_task,
        Some(&frozen.digest),
        checked.require_result.as_deref(),
        Some(&source_revision),
    );
    if invocation.auto_task != checked.auto_task
        || (record.minted && invocation.action_key != record.action_key)
        || invocation.input_digest != record.input_digest
        || invocation.input_digest != expected_digest
    {
        return Err(format!(
            "task {} was not minted for this definition digest and source revision",
            task.id
        ));
    }
    let completed_at = trusted_completion_time(&history)
        .ok_or_else(|| format!("task {} has no recorded completion", task.id))?;
    let (run_id, execution_machine) = trusted_run(runtime, task)?;
    let workspace_id = runtime
        .workspace_id()
        .map_err(|error| format!("the workspace identity is unreadable: {error}"))?;
    Ok(TrustedIdentity {
        binding: AutoTaskEvidenceBinding {
            workspace_id,
            definition: checked.auto_task.clone(),
            definition_digest: frozen.digest.clone(),
            task_id: task.id.clone(),
            run_id,
            source_revision,
        },
        completed_at,
        execution_machine,
    })
}

/// The owner's recorded transition to done. A task's `updated_at` moves on any
/// later edit, so it never stands in for this.
fn trusted_completion_time(history: &[TaskHistoryEntry]) -> Option<DateTime<Utc>> {
    history
        .iter()
        .rev()
        .find(|entry| entry.to_status == Some(TaskStatus::Done))
        .map(|entry| entry.at)
}

/// The run that executed the task. The stored id alone proves nothing, as a
/// task update can set it, so a local run must carry this task in its own
/// input. Another host's run has no record here; its claim names the machine.
fn trusted_run(runtime: &OrbitRuntime, task: &Task) -> Result<(String, Option<String>), String> {
    let run_id = task
        .job_run_id
        .clone()
        .filter(|id| !id.trim().is_empty())
        .ok_or_else(|| format!("task {} has no execution run", task.id))?;
    if !runtime.task_run_is_local(task) {
        let machine = task
            .job_run_machine
            .as_ref()
            .map(|at| at.machine_id.clone());
        return Ok((run_id, machine));
    }
    let run = runtime
        .get_job_run_backend(&run_id)
        .map_err(|error| format!("run {run_id} is unreadable: {error}"))?
        .ok_or_else(|| format!("run {run_id} of task {} does not exist", task.id))?;
    let carries_task = run
        .input
        .as_ref()
        .is_some_and(|input| carried_task_ids(input).contains(&task.id));
    if !carries_task {
        return Err(format!("run {run_id} did not execute task {}", task.id));
    }
    Ok((run_id, None))
}

type AdmittedEvidence = (
    AutoTaskEvidenceCandidate,
    Vec<AutoTaskEvidenceRef>,
    Map<String, Value>,
);

/// Read the candidate evidence and compare it with the owner's records. Every
/// artifact must already exist when the task settled: evidence written later
/// cannot support a completion that happened without it.
fn admitted_evidence(
    runtime: &OrbitRuntime,
    contract: &AutoTaskCompletionContract,
    trusted: &TrustedIdentity,
) -> Result<AdmittedEvidence, String> {
    let task_id = &trusted.binding.task_id;
    let unavailable =
        |why: String| format!("`{}` evidence is unavailable: {why}", contract.contract);
    let manifest = runtime
        .get_task_artifact_manifest(task_id)
        .map_err(|error| unavailable(format!("the artifacts are unreadable: {error}")))?;
    let on_time = |path: &str| -> Result<&ArtifactManifestFileV2, String> {
        let file = manifest
            .iter()
            .find(|file| file.path == path)
            .ok_or_else(|| unavailable(format!("artifact `{path}` is missing")))?;
        if file.created_at > trusted.completed_at {
            return Err(unavailable(format!(
                "artifact `{path}` was written after the task completed"
            )));
        }
        Ok(file)
    };
    let manifest_file = on_time(&contract.evidence_artifact)?;
    let read = |path: &str| {
        runtime
            .get_task_artifact(task_id, path)
            .map_err(|error| unavailable(format!("artifact `{path}` is unreadable: {error}")))?
            .ok_or_else(|| unavailable(format!("artifact `{path}` is missing")))
    };
    let raw = read(&contract.evidence_artifact)?;
    let candidate: AutoTaskEvidenceCandidate = serde_json::from_slice(&raw.content)
        .map_err(|error| unavailable(format!("the evidence manifest is malformed: {error}")))?;
    if candidate.schema_version != AUTO_TASK_EVIDENCE_SCHEMA_VERSION {
        return Err(unavailable(format!(
            "the evidence manifest has schema_version {}",
            candidate.schema_version
        )));
    }
    if candidate.contract != contract.contract {
        return Err(unavailable(format!(
            "the evidence manifest is for contract `{}`",
            candidate.contract
        )));
    }
    if let Some(field) = binding_mismatch(&candidate.binding, &trusted.binding) {
        return Err(unavailable(format!(
            "the evidence manifest's {field} does not match the owner's record"
        )));
    }
    if candidate.artifacts.is_empty() {
        return Err(unavailable(
            "the evidence manifest lists nothing".to_string(),
        ));
    }
    let mut evidence = vec![AutoTaskEvidenceRef {
        path: manifest_file.path.clone(),
        sha256: manifest_file.sha256.clone(),
    }];
    let mut artifacts = Map::new();
    for listed in &candidate.artifacts {
        let file = on_time(&listed.path)?;
        if file.sha256 != listed.sha256 {
            return Err(unavailable(format!(
                "artifact `{}` does not match its listed digest",
                listed.path
            )));
        }
        let content = read(&listed.path)?.content;
        if sha256_hex(&content) != listed.sha256 {
            return Err(unavailable(format!(
                "artifact `{}` does not match its stored digest",
                listed.path
            )));
        }
        artifacts.insert(
            listed.path.clone(),
            json!({
                "sha256": listed.sha256,
                "json": serde_json::from_slice::<Value>(&content).ok(),
            }),
        );
        evidence.push(listed.clone());
    }
    Ok((candidate, evidence, artifacts))
}

fn binding_mismatch(
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

/// A task as an adapter reads it, from the owner's record.
fn task_fact(task: &Task) -> Value {
    json!({
        "id": task.id,
        "title": task.title,
        "status": task.status,
        "priority": task.priority,
        "type": task.task_type,
        "tags": task.tags,
        "description": task.description,
        "context_files": task.context_files,
        "relations": task.relations.iter().map(|relation| json!({
            "type": relation.relation_type,
            "target": relation.target,
        })).collect::<Vec<_>>(),
    })
}

/// The tasks a policy may judge: those spawned from the bound task and those
/// carrying a declared tag, listed by the owner so evidence cannot omit one,
/// and those the evidence cites, read back so it cannot misstate one.
fn trusted_task_facts(
    runtime: &OrbitRuntime,
    contract: &AutoTaskCompletionContract,
    candidate: &AutoTaskEvidenceCandidate,
    task: &Task,
) -> Result<Value, String> {
    let all = runtime
        .list_tasks()
        .map_err(|error| format!("the task board is unreadable: {error}"))?;
    let spawned: Vec<Value> = all
        .iter()
        .filter(|other| {
            other.relations.iter().any(|relation| {
                relation.relation_type == orbit_types::task::TaskRelationType::SpawnedFrom
                    && relation.target == task.id
            })
        })
        .map(task_fact)
        .collect();
    let tagged: Vec<Value> = all
        .iter()
        .filter(|other| {
            other
                .tags
                .iter()
                .any(|tag| contract.read_task_tags.contains(tag))
        })
        .map(task_fact)
        .collect();
    let cited: BTreeSet<&String> = candidate.task_refs.iter().collect();
    let referenced: Vec<Value> = cited
        .into_iter()
        .map(|id| {
            all.iter()
                .find(|other| &other.id == id)
                .map_or_else(|| json!({"id": id, "status": "missing"}), task_fact)
        })
        .collect();
    Ok(json!({
        "bound": task_fact(task),
        "spawned": spawned,
        "tagged": tagged,
        "referenced": referenced,
    }))
}

/// Independently verified pinned release survey for a `release-prep`
/// adapter: the latest reachable tag and no-merge range come from Git at the
/// pinned revision, and the breaking classification from owner task records
/// (a surveyed commit whose subject names a task the owner records with the
/// `breaking-change` tag). Candidate evidence is never consulted, so a
/// handoff and a canonical task that merely agree with each other cannot
/// invent a tag, a range or a version. A Git failure holds the step.
fn trusted_release_survey(runtime: &OrbitRuntime, pinned: &str) -> Result<Value, String> {
    let source = Source::new(&runtime.paths().repo_root);
    let survey = source
        .release_survey(pinned)
        .map_err(|error| format!("the pinned release survey is unavailable: {error}"))?;
    let tasks = runtime
        .list_tasks()
        .map_err(|error| format!("the task board is unreadable: {error}"))?;
    let breaking = survey.commits.iter().any(|commit| {
        surveyed_task_ids(&commit.subject).iter().any(|id| {
            tasks.iter().any(|task| {
                task.id.as_str() == id.as_str()
                    && task
                        .tags
                        .iter()
                        .any(|tag| tag.as_str() == "breaking-change")
            })
        })
    });
    Ok(json!({
        "latest_tag": survey.latest_tag,
        "head": pinned,
        "commits": survey.commits.iter().map(|commit| &commit.sha).collect::<Vec<_>>(),
        "breaking": breaking,
    }))
}

/// Task IDs a commit subject names, in `[ORB-123]` bracket form.
fn surveyed_task_ids(subject: &str) -> Vec<String> {
    let mut ids = Vec::new();
    let mut rest = subject;
    while let Some(open) = rest.find('[') {
        let after = &rest[open + 1..];
        let Some(close) = after.find(']') else { break };
        let candidate = &after[..close];
        if let Some((prefix, number)) = candidate.rsplit_once('-')
            && !prefix.is_empty()
            && prefix
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '-')
            && !number.is_empty()
            && number.chars().all(|c| c.is_ascii_digit())
        {
            ids.push(candidate.to_string());
        }
        rest = &after[close + 1..];
    }
    ids
}

fn run_adapter(
    runtime: &OrbitRuntime,
    step: &StepContext,
    checked: &CheckedAutoTaskStep,
    contract: &AutoTaskCompletionContract,
    trusted: &TrustedIdentity,
    evidence: (&AutoTaskEvidenceCandidate, &Map<String, Value>),
    task: &Task,
) -> Result<(AutoTaskAdapterVerdict, String), String> {
    let (candidate, artifacts) = evidence;
    let unavailable = |why: String| {
        format!(
            "`{}` evidence is unavailable: adapter `{}` {why}",
            contract.contract, contract.adapter
        )
    };
    let tasks = trusted_task_facts(runtime, contract, candidate, task).map_err(&unavailable)?;
    // Only release preparation needs pinned survey facts; obtaining them is
    // read-only Git and owner records, never candidate evidence.
    let release_survey = if contract.form == "release-prep" {
        Some(
            trusted_release_survey(runtime, &trusted.binding.source_revision)
                .map_err(&unavailable)?,
        )
    } else {
        None
    };
    let input = json!({
        "schema_version": AUTO_TASK_EVIDENCE_SCHEMA_VERSION,
        "contract": contract.contract,
        "form": contract.form,
        "trusted": {
            "workspace_id": trusted.binding.workspace_id,
            "definition": trusted.binding.definition,
            "definition_digest": trusted.binding.definition_digest,
            "task_id": trusted.binding.task_id,
            "run_id": trusted.binding.run_id,
            "source_revision": trusted.binding.source_revision,
            "completed_at": trusted.completed_at,
            "execution_machine": trusted.execution_machine,
            "release_survey": release_survey,
        },
        "evidence": {"manifest": candidate, "artifacts": artifacts},
        "tasks": tasks,
    });
    let request = ScriptStepRequest {
        run_id: step.state.run_id.clone(),
        step_id: checked.id.clone(),
        script: contract.adapter.clone(),
        input,
        workspace_path: None,
        timeout_seconds: None,
    };
    let ran = match run_completion_adapter(runtime, &request) {
        ScriptStepRun::Ran(ran) => ran,
        ScriptStepRun::Refused { code, message, .. } => {
            return Err(unavailable(format!("was refused ({code}): {message}")));
        }
    };
    if ran.end != ScriptStepEnd::Exited(0) {
        return Err(unavailable(format!(
            "did not finish cleanly: {:?}",
            ran.end
        )));
    }
    let output = script_stdout_output(&ran.stdout, ran.stdout_over_cap, &ran.output_schema)
        .map_err(|error| unavailable(format!("gave no usable verdict: {error}")))?;
    let verdict: AutoTaskAdapterVerdict = serde_json::from_value(output)
        .map_err(|error| unavailable(format!("gave a malformed verdict: {error}")))?;
    if verdict.schema_version != AUTO_TASK_EVIDENCE_SCHEMA_VERSION
        || verdict.contract != contract.contract
        || verdict.form != contract.form
        || verdict.task_id != trusted.binding.task_id
        || verdict.source_revision != trusted.binding.source_revision
    {
        return Err(unavailable("answered about something else".to_string()));
    }
    Ok((verdict, ran.record.digest))
}

fn run_completion_adapter(runtime: &OrbitRuntime, request: &ScriptStepRequest) -> ScriptStepRun {
    #[cfg(feature = "test-support")]
    if let Some(ran) = super::completion_test_support::completion_adapter_run(request) {
        return ran;
    }
    run_script_step(runtime, request)
}

/// The instructions a contract adds to a task it mints: what the owner will
/// compare the evidence with. The task and run ids are the worker's own.
pub(super) fn evidence_binding_note(
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

/// Advance a definition's accepted-result history. Only a settled record that
/// carries a passed result reaches it, so a successful probe outcome, which the
/// adapter never passes, cannot. Time only moves forward.
pub(super) fn record_accepted_result(
    runtime: &OrbitRuntime,
    definition: &str,
    result: &AutoTaskCompletionReceipt,
) -> Result<(), OrbitError> {
    if result.status != AutoTaskResultStatus::Passed {
        return Ok(());
    }
    let path = accepted_history_path(runtime);
    with_exclusive_file_lock(&path, "auto-task accepted results", || {
        let mut history = read_accepted_history_file(&path)?.unwrap_or_default();
        let current = history.definitions.get(definition);
        if current.is_some_and(|current| current.completed_at >= result.completed_at) {
            return Ok(());
        }
        history
            .definitions
            .insert(definition.to_string(), result.clone());
        let encoded = serde_json::to_string_pretty(&history)
            .map_err(|error| OrbitError::Io(format!("encode accepted results: {error}")))?;
        atomic_write_text(&path, &encoded).map_err(|error| OrbitError::from_write_io(&path, error))
    })
}

fn read_accepted_history_file(
    path: &std::path::Path,
) -> Result<Option<AutoTaskAcceptedHistoryFile>, OrbitError> {
    let unreadable =
        |error: std::io::Error| OrbitError::Io(format!("read {}: {error}", path.display()));
    // Not following the final component, as for the schedule cursor beside it.
    let mut file = match open_read_only_no_follow(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(unreadable(error)),
    };
    let mut raw = String::new();
    file.read_to_string(&mut raw).map_err(unreadable)?;
    let history: AutoTaskAcceptedHistoryFile = serde_json::from_str(&raw)
        .map_err(|error| OrbitError::Io(format!("parse {}: {error}", path.display())))?;
    if history.schema_version != AUTO_TASK_ACCEPTED_HISTORY_SCHEMA_VERSION {
        return Err(OrbitError::Io(format!(
            "{} has schema_version {}",
            path.display(),
            history.schema_version
        )));
    }
    Ok(Some(history))
}

/// A definition's latest accepted result. History lives with the owner, so a
/// replica checkout answers unreadable rather than a misleading missing.
pub(super) fn accepted_history(runtime: &OrbitRuntime, name: &str) -> AutoTaskAcceptedHistory {
    if let Some(owner) = runtime.coordination_write_owner() {
        return AutoTaskAcceptedHistory::Unreadable(format!(
            "accepted results live with the owner {owner}, not this replica"
        ));
    }
    match read_accepted_history_file(&accepted_history_path(runtime)) {
        Ok(None) => AutoTaskAcceptedHistory::Missing,
        Ok(Some(mut history)) => match history.definitions.remove(name) {
            None => AutoTaskAcceptedHistory::Missing,
            Some(receipt) if receipt.definition != name => AutoTaskAcceptedHistory::Unreadable(
                format!("the entry for `{name}` names `{}`", receipt.definition),
            ),
            Some(receipt) if receipt.status != AutoTaskResultStatus::Passed => {
                AutoTaskAcceptedHistory::Unreadable(format!(
                    "the entry for `{name}` is {}, which is never accepted",
                    receipt.status.as_str()
                ))
            }
            Some(receipt) => AutoTaskAcceptedHistory::Latest(Box::new(receipt)),
        },
        Err(error) => AutoTaskAcceptedHistory::Unreadable(error.to_string()),
    }
}
