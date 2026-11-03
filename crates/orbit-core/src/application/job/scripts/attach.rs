//! `attach_script_files`: attach a prior script step's declared output
//! files to a task as task artifacts (script steps §4 Files out, §9 item 4).
//!
//! Input: `task_id` (the task to attach to), `step` (the id of a direct,
//! earlier script step of the same job), optional `files` (the names to
//! attach; every file of the set when absent) and optional `path_prefix`
//! (prepended to each name to form the artifact path).
//!
//! The set is found by the step id in the consuming run's own checkpoint,
//! never taken from the input, and honoured only through
//! [`verify_script_output_set`]: the run must be the producer or resume it,
//! the producer record must match and every file must re-hash. Each file is
//! then read once more from its store path and attached from exactly those
//! bytes, which must match the recorded SHA-256. Any refusal attaches
//! nothing.

use orbit_common::OrbitError;
use orbit_types::task::{MAX_TASK_ARTIFACT_CONTENT_BYTES, TaskArtifact};
use orbit_types::workflow::activity_job::{SCRIPT_REF_PREFIX, ScriptOutputSet};
use orbit_types::workflow::{JobRunState, JobV2StepBody};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::job::script_outputs::{
    read_verified_script_output, verify_script_output_set,
};
use crate::application::task::TaskUpdateParams;

const ACTION: &str = "attach_script_files";

fn refused(detail: impl std::fmt::Display) -> OrbitError {
    OrbitError::InvalidInput(format!("{ACTION}: {detail}"))
}

/// Attach the named step's output files to `task_id`. `consumer_run_id`
/// is the host-attested run dispatching this step.
pub(crate) fn attach_script_files(
    runtime: &OrbitRuntime,
    input: &Value,
    consumer_run_id: Option<&str>,
) -> Result<Value, OrbitError> {
    let task_id = required_string(input, "task_id")?;
    let step = required_string(input, "step")?;
    let prefix = match input.get("path_prefix") {
        None | Some(Value::Null) => "",
        Some(Value::String(prefix)) => prefix.as_str(),
        Some(_) => return Err(refused("path_prefix must be a string")),
    };
    let run_id =
        consumer_run_id.ok_or_else(|| refused("needs a host-attested consuming job run"))?;
    let set = producer_output_set(runtime, run_id, step)?;
    verify_script_output_set(runtime, run_id, &set)
        .map_err(|reason| refused(format!("the output set of step `{step}` {reason}")))?;

    let selected: Vec<_> = match input.get("files") {
        None | Some(Value::Null) => set.files.iter().collect(),
        Some(Value::Array(names)) => {
            let mut selected = Vec::with_capacity(names.len());
            for name in names {
                let name = name
                    .as_str()
                    .ok_or_else(|| refused("files must be a list of output file names"))?;
                let file = set
                    .files
                    .iter()
                    .find(|file| file.name == name)
                    .ok_or_else(|| {
                        refused(format!("`{name}` is not an output file of step `{step}`"))
                    })?;
                if !selected.iter().any(|kept: &&_| std::ptr::eq(*kept, file)) {
                    selected.push(file);
                }
            }
            selected
        }
        Some(_) => return Err(refused("files must be a list of output file names")),
    };

    let mut artifacts = Vec::with_capacity(selected.len());
    let mut attached = Vec::with_capacity(selected.len());
    for file in selected {
        let content =
            read_verified_script_output(runtime, &set, file, MAX_TASK_ARTIFACT_CONTENT_BYTES)
                .map_err(|reason| refused(format!("step `{step}` {reason}")))?;
        let mut artifact = TaskArtifact {
            path: format!("{prefix}{}", file.name),
            content,
            media_type: file.media_type.clone(),
            created_by: Some("system".to_string()),
        };
        crate::application::task::admit_agent_artifact(&mut artifact)?;
        attached.push(json!({
            "name": file.name,
            "path": artifact.path,
            "bytes": file.bytes,
            "media_type": artifact.media_type,
            "sha256": file.sha256,
        }));
        artifacts.push(artifact);
    }
    if !artifacts.is_empty() {
        runtime.update_task(
            task_id,
            TaskUpdateParams {
                upsert_artifacts: artifacts,
                ..Default::default()
            },
        )?;
    }
    Ok(json!({
        "task_id": task_id,
        "step": step,
        "set_id": set.set_id,
        "attached": attached,
    }))
}

fn required_string<'a>(input: &'a Value, field: &str) -> Result<&'a str, OrbitError> {
    input
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| refused(format!("{field} must be a non-empty string")))
}

/// The output set the run's checkpoint holds for `step`: a unique, direct
/// script step that precedes the current one and succeeded.
fn producer_output_set(
    runtime: &OrbitRuntime,
    run_id: &str,
    step: &str,
) -> Result<ScriptOutputSet, OrbitError> {
    let state = runtime
        .read_run_state(run_id)?
        .ok_or_else(|| refused(format!("run {run_id} has no checkpoint")))?;
    let (job, _) = runtime
        .read_run_definition_snapshot(run_id)?
        .ok_or_else(|| refused(format!("run {run_id} has no job definition snapshot")))?;
    let matches: Vec<_> = job
        .steps
        .iter()
        .enumerate()
        .filter(|(_, candidate)| candidate.id == step)
        .collect();
    let [(index, producer)] = matches.as_slice() else {
        return Err(refused(format!(
            "step `{step}` must name one direct step of the job"
        )));
    };
    if !matches!(&producer.body, JobV2StepBody::TargetRef(target)
        if target.target.starts_with(SCRIPT_REF_PREFIX))
    {
        return Err(refused(format!("step `{step}` is not a script step")));
    }
    let index = u32::try_from(*index).map_err(|_| refused("step index is out of range"))?;
    if index >= state.next_step_index {
        return Err(refused(format!("step `{step}` does not precede this step")));
    }
    if state.step_states.get(&index) != Some(&JobRunState::Success) {
        return Err(refused(format!("step `{step}` did not succeed")));
    }
    let set = state
        .script_output_sets
        .get(&index)
        .and_then(|sets| sets.get(step))
        .ok_or_else(|| refused(format!("step `{step}` kept no output files")))?;
    if set.step_id != step || set.run_id.is_empty() {
        return Err(refused(format!(
            "the checkpointed output set does not belong to step `{step}`"
        )));
    }
    Ok(set.clone())
}
