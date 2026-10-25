//! Validation evidence and the bounded cache of validated trees.

use orbit_common::OrbitError;
use orbit_engine::RuntimeHost;

use crate::OrbitRuntime;

/// Runs whose last validated tree one process keeps [ORB-16784].
const MAX_VALIDATED_TREE_RUNS: usize = 256;

pub(super) fn attach_claim_validation_log(
    runtime: &OrbitRuntime,
    path: &str,
    content: Vec<u8>,
) -> Result<(), OrbitError> {
    let leaf = runtime.current_claimed_leaf()?;
    orbit_types::task::validate_relative_artifact_path(path)?;
    // The same preloaded payload the spoke connector sends: bytes are read
    // on the executor and cross the coordination seam path-free, so the
    // evidence the owner later re-reads by digest lives in the owner's
    // store rather than on the executor's disk.
    runtime
        .route_worker_tool(
            "orbit.task.artifact.put",
            serde_json::json!({
                "id": leaf.claim.task_id,
                "artifacts": [{
                    "path": path,
                    "content": content,
                    "media_type": orbit_types::task::media_type_for_artifact_path(path),
                }],
            }),
            Default::default(),
        )
        .map(|_| ())
}

pub(super) fn attach_task_validation_log(
    runtime: &OrbitRuntime,
    task_id: &str,
    run_id: &str,
    path: &str,
    content: Vec<u8>,
) -> Result<(), OrbitError> {
    orbit_types::task::validate_relative_artifact_path(path)?;
    let task = runtime.get_task(task_id)?;
    if task.job_run_id.as_deref() != Some(run_id) {
        return Err(OrbitError::PolicyDenied(format!(
            "run '{run_id}' does not own task '{task_id}'; it cannot attach validation evidence"
        )));
    }
    runtime
        .update_task_as_system(
            task_id,
            crate::application::task::TaskUpdateParams {
                upsert_artifacts: vec![orbit_types::task::TaskArtifact {
                    path: path.to_string(),
                    content,
                    media_type: "application/json".to_string(),
                    created_by: None,
                }],
                ..Default::default()
            },
            Some(run_id.to_string()),
        )
        .map(|_| ())
}

pub(super) fn record_flaky_validation(
    runtime: &OrbitRuntime,
    task_id: &str,
    record: &orbit_types::workflow::FlakyValidation,
) -> Result<(), OrbitError> {
    crate::application::review::flaky::record_frictions(runtime, task_id, record)?;
    runtime.apply_task_automation_update(
        task_id,
        orbit_engine::TaskAutomationUpdate {
            append_history: vec![orbit_types::task::TaskHistoryEntry {
                at: chrono::Utc::now(),
                by: "system".into(),
                event: orbit_types::workflow::FLAKY_VALIDATION_EVENT.into(),
                note: Some(record.text()),
                from_status: None,
                to_status: None,
                operation_id: None,
                decision: Some(orbit_types::task::TaskDecision::FlakyValidation(
                    record.clone(),
                )),
            }],
            ..Default::default()
        },
    )
}

pub(super) fn record_validated_tree(
    runtime: &OrbitRuntime,
    tree: &orbit_engine::ValidatedTree,
) -> Result<(), OrbitError> {
    // A claimed leaf's validation is its owner's evidence, not this host's.
    if runtime.worker_invocation().is_some() {
        return Ok(());
    }
    let mut trees = runtime
        .validated_trees
        .lock()
        .map_err(|_| OrbitError::Execution("validated tree record is poisoned".into()))?;
    trees.insert(tree.run_id.clone(), tree.clone());
    // A long-lived host serves many runs; run IDs sort by start time.
    while trees.len() > MAX_VALIDATED_TREE_RUNS {
        trees.pop_first();
    }
    Ok(())
}

pub(super) fn validated_tree(
    runtime: &OrbitRuntime,
    run_id: &str,
) -> Result<Option<orbit_engine::ValidatedTree>, OrbitError> {
    if runtime.worker_invocation().is_some() {
        return Ok(None);
    }
    Ok(runtime
        .validated_trees
        .lock()
        .map_err(|_| OrbitError::Execution("validated tree record is poisoned".into()))?
        .get(run_id)
        .cloned())
}

pub(super) fn unlocked_path_validation(
    runtime: &OrbitRuntime,
) -> orbit_engine::UnlockedPathValidation {
    let settings = runtime.context.settings();
    orbit_engine::UnlockedPathValidation {
        paths: settings.unlocked_context_paths().patterns().to_vec(),
        commands: settings.unlocked_path_validation_commands().to_vec(),
    }
}

pub(super) fn generated_paths(runtime: &OrbitRuntime) -> Vec<orbit_engine::GeneratedPath> {
    runtime
        .context
        .settings()
        .generated_paths()
        .iter()
        .map(|(path, command)| orbit_engine::GeneratedPath {
            path: path.clone(),
            command: command.clone(),
        })
        .collect()
}

pub(super) fn agent_stall_window(runtime: &OrbitRuntime) -> Option<std::time::Duration> {
    let minutes = runtime.context.agent_stall_minutes();
    (minutes > 0).then(|| std::time::Duration::from_secs(u64::from(minutes) * 60))
}
