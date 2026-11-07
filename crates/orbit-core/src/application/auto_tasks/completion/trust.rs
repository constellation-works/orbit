//! Trusted completion identity and admitted evidence from owner records.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use orbit_automation::condition::CheckedAutoTaskStep;
use orbit_common::security::release::sha256_hex;
use orbit_types::task::{ArtifactManifestFileV2, Task, TaskHistoryEntry, TaskStatus};
use orbit_types::workflow::{
    AUTO_TASK_EVIDENCE_SCHEMA_VERSION, AutoTaskCompletionContract, AutoTaskEvidenceBinding,
    AutoTaskEvidenceCandidate, AutoTaskEvidenceRef, AutoTaskStepRecord,
};
use serde_json::{Map, Value, json};

use super::super::step_observation::{StepContext, step_input_digest};
use super::binding;
use crate::OrbitRuntime;
use crate::application::automation::source::Source;
use crate::runtime::run_input::carried_task_ids;

/// What the owner's records say this completion is.
pub(super) struct TrustedIdentity {
    pub(super) binding: AutoTaskEvidenceBinding,
    pub(super) completed_at: DateTime<Utc>,
    /// The machine the claim record names, when the run lives on another host.
    pub(super) execution_machine: Option<String>,
}

pub(super) fn trusted_identity(
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
    let invocation = super::super::job_step::creation_receipt(&history).ok_or_else(|| {
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
pub(super) fn trusted_completion_time(history: &[TaskHistoryEntry]) -> Option<DateTime<Utc>> {
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

pub(super) type AdmittedEvidence = (
    AutoTaskEvidenceCandidate,
    Vec<AutoTaskEvidenceRef>,
    Map<String, Value>,
);

/// Read the candidate evidence and compare it with the owner's records. Every
/// artifact must already exist when the task settled: evidence written later
/// cannot support a completion that happened without it.
pub(super) fn admitted_evidence(
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
    if let Some(field) = binding::binding_mismatch(&candidate.binding, &trusted.binding) {
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
pub(super) fn trusted_task_facts(
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
pub(super) fn trusted_release_survey(
    runtime: &OrbitRuntime,
    pinned: &str,
) -> Result<Value, String> {
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
