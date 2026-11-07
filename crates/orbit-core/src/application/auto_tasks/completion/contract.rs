//! Completion-contract resolution and admission holds.

use chrono::{DateTime, Duration, Utc};
use orbit_automation::condition::CheckedAutoTaskStep;
use orbit_types::task::TaskStatus;
use orbit_types::workflow::{
    AutoTaskCompletionContract, AutoTaskCompletionReceipt, AutoTaskDefinition,
    AutoTaskResultStatus, AutoTaskStepRecord,
};

use super::super::step_observation::StepContext;
use super::{binding, trust};
use crate::OrbitRuntime;
use crate::application::automation::source::Source;
use crate::runtime::scripts::ScriptGrantState;

/// How far a recorded proof is rechecked. Reading records is cheap enough for
/// every pass; asking the adapter again is reserved for the admission fence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::application::auto_tasks) enum ProofDepth {
    Records,
    Adapter,
}

/// The contract this step is held to: `None` for an ordinary step on a
/// definition that declares none. A contract on only one side, or two
/// different ones, can never produce a result, so the step holds.
pub(super) fn declared_contract(
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
pub(in crate::application::auto_tasks) fn contract_admission_hold(
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
pub(in crate::application::auto_tasks) fn prior_proof_hold(
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
        || trust::trusted_completion_time(&history).as_ref() != Some(&result.completed_at)
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
        binding::validate_completion(runtime, step, other, record, &contract, &task, now)?;
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
pub(in crate::application::auto_tasks) fn reused_result(
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
        binding::validate_completion(runtime, step, &step.checked, record, &contract, &task, now)?;
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
