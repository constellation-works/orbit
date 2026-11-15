//! Import a recovery's evidence under the host's admitted task authority.

use std::collections::BTreeSet;
use std::path::Path;

use orbit_common::OrbitError;
use orbit_engine::RuntimeHost;
use orbit_types::task::{TaskArtifact, canonical_artifact_path};
use orbit_types::workflow::{JobRunState, is_reserved_review_artifact};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::task::{TaskUpdateParams, admit_agent_artifact};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RecoveryEvidence {
    execution_summary: String,
    #[serde(default)]
    task_artifacts: Vec<ScratchArtifact>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScratchArtifact {
    path: String,
    source_path: String,
}

fn refused(message: impl Into<String>) -> OrbitError {
    OrbitError::PolicyDenied(format!(
        "final_recovery_evidence_refused: {}",
        message.into()
    ))
}

pub(super) fn persist_final_recovery_evidence(
    runtime: &OrbitRuntime,
    run_id: &str,
    task_id: &str,
    workspace: &Path,
    evidence: &Value,
) -> Result<(), OrbitError> {
    let run = runtime
        .get_job_run(run_id)?
        .ok_or_else(|| refused("the run does not exist"))?;
    let running = run.state == JobRunState::Running;
    let admitted = runtime.read_run_state(run_id)?.is_some_and(|state| {
        state.final_recovery.is_some_and(|checkpoint| {
            checkpoint.key.run_id == run_id
                && checkpoint.task_id == task_id
                && checkpoint.decision.is_none()
                && checkpoint.outcome.is_none()
        })
    });
    if !running || !admitted {
        return Err(refused(
            "the run has no undecided final recovery for this task",
        ));
    }
    if runtime.worker_invocation().is_some() {
        let leaf = runtime.claimed_leaf_in_final_recovery(run_id)?;
        if leaf.claim.task_id != task_id {
            return Err(refused("the task is not the claimed task"));
        }
    } else {
        let task = runtime.get_task(task_id)?;
        let owns_task = match task.job_run_id.as_deref() {
            Some(owner) => runtime.recovery_run_descends_from(run.clone(), owner)?,
            None => false,
        };
        if task.status.is_terminal() || !owns_task {
            return Err(refused("the run no longer owns a writable task"));
        }
    }
    let evidence: RecoveryEvidence = serde_json::from_value(evidence.clone())
        .map_err(|error| refused(format!("invalid evidence: {error}")))?;
    if evidence.execution_summary.trim().is_empty()
        || evidence.execution_summary.chars().count() > 65536
        || evidence.task_artifacts.len() > 32
    {
        return Err(refused(
            "evidence requires a nonempty summary of at most 65536 characters and at most 32 artifacts",
        ));
    }
    let mut paths = BTreeSet::new();
    let mut artifacts = Vec::new();
    let scratch = workspace.join(".orbit/tmp");
    if !evidence.task_artifacts.is_empty()
        && !scratch
            .canonicalize()?
            .starts_with(workspace.canonicalize()?)
    {
        return Err(refused(
            "the scratch directory leaves the assigned worktree",
        ));
    }
    // Prepare and admit the entire batch before writing anything. In particular,
    // a later bad source must not leave an apparently complete earlier summary.
    for reference in evidence.task_artifacts {
        let path = canonical_artifact_path(&reference.path)?;
        if is_reserved_review_artifact(&path)
            || path.starts_with("validation/")
            || path.starts_with("no-diff-handoff/")
            || !paths.insert(path.clone())
        {
            return Err(refused(format!(
                "reserved or duplicate artifact path '{path}'"
            )));
        }
        let payload = orbit_tools::prepare_remote_task_artifact_put(
            json!({"id": task_id, "path": path, "source_path": reference.source_path}),
            Some(workspace),
            Some(&scratch),
            None,
        )?;
        let mut artifact: TaskArtifact = serde_json::from_value(payload["artifacts"][0].clone())
            .map_err(|error| refused(format!("invalid prepared artifact: {error}")))?;
        admit_agent_artifact(&mut artifact)?;
        artifacts.push(artifact);
    }
    if runtime.worker_invocation().is_some() {
        // Only the trusted host path can carry a worker update. It sends bytes,
        // never executor-local paths; the owner's claim fence rechecks task ID.
        if !artifacts.is_empty() {
            runtime.route_worker_tool(
                "orbit.task.artifact.put",
                json!({
                    "id": task_id, "artifacts": artifacts,
                }),
                Default::default(),
            )?;
        }
        runtime.route_worker_host_tool(
            "orbit.task.update",
            json!({
                "id": task_id,
                "_worker_update": orbit_store::contracts::ClaimWorkerUpdate {
                    evidence: orbit_store::contracts::ClaimEvidence {
                        summary: Some(evidence.execution_summary), ..Default::default()
                    },
                    ..Default::default()
                }
            }),
        )?;
    } else {
        let mut update = Some(TaskUpdateParams {
            execution_summary: Some(evidence.execution_summary),
            upsert_artifacts: artifacts,
            ..Default::default()
        });
        runtime
            .stores()
            .tasks()
            .with_task_write_lock(task_id, &mut || {
                let task = runtime.get_task(task_id)?;
                let owns_task = match task.job_run_id.as_deref() {
                    Some(owner) => runtime.recovery_run_descends_from(run.clone(), owner)?,
                    None => false,
                };
                if task.status.is_terminal() || !owns_task {
                    return Err(refused("the run no longer owns a writable task"));
                }
                let params = update
                    .take()
                    .ok_or_else(|| refused("evidence write invoked twice"))?;
                runtime.update_task_as_system(task_id, params, Some(run_id.to_string()))?;
                Ok(())
            })?;
    }
    Ok(())
}
