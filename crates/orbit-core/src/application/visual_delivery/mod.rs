//! The delivery's visual review phase (visual-review design §4, §6–§11):
//! for a `verify-mock-ui` task whose `visual_review.mode` is not `off`,
//! capture the final candidate once per mock in a detached checkout
//! (`visual_capture`), record the deterministic checks (`visual_checks`),
//! run the mock comparison review and its in-line verification in shadow
//! (`visual_review_prepare`, the `visual_review` and `visual_verify`
//! agents, `visual_verify_prepare`), and settle the capture against the
//! candidate and the frozen mocks (`visual_settle`, which also writes
//! `visual/review.json`). The system writer owns the reserved `visual/`
//! prefix. Live delivery resolves confirmed findings and establishes the
//! final candidate before every landing attempt; shadow records only.
//!
//! A shadow claimed leaf captures against the visual review contract its owner
//! captured at admission, and the owner checks what the leaf returns before
//! it accepts the handoff (`visual_capture_mismatch`). Negotiated live PR
//! claims additionally return a resolution bundle; the owner checks complete
//! exact-head captures, review resolutions and operator history on acceptance
//! and every landing authority decision. Unsupported routes remain held.

mod capture;
mod checks;
mod claimed;
mod contract;
mod distributed_port;
mod handoff;
mod landed;
mod resolve;
mod review;
mod review_port;
mod settle;
#[cfg(test)]
mod tests;
mod verify;

pub(crate) use capture::{visual_capture_step, visual_measurement_landed_revision};
pub(crate) use checks::visual_checks_step;
pub(crate) use landed::visual_landed_step;
pub(crate) use resolve::visual_recapture_step;
pub(crate) use review::visual_review_prepare_step;
pub(crate) use settle::visual_settle_step;
pub(crate) use verify::visual_verify_prepare_step;

use orbit_common::OrbitError;
use orbit_engine::DispatchError;
use orbit_types::task::{TaskArtifact, media_type_for_artifact_path};
use serde_json::Value;

use crate::OrbitRuntime;
use crate::application::task::TaskUpdateParams;
// The record read lives in `task` [ORB-22624]; this alias is a shim for the
// children's `super::read_record` imports, whose retirement is a follow-up.
use crate::application::task::read_visual_record as read_record;

/// The pass a step runs for: its input's `pass`, else the `pass` its
/// capture output or capture entry names, else the delivery's.
fn step_pass(input: &Value) -> orbit_types::task::VisualPass {
    [
        input.get("pass"),
        input.get("capture").and_then(|capture| capture.get("pass")),
    ]
    .into_iter()
    .flatten()
    .find_map(|pass| pass.as_str().and_then(orbit_types::task::VisualPass::parse))
    .unwrap_or_default()
}

/// The task ids a delivery step acts for.
fn step_task_ids(input: &Value) -> Vec<String> {
    input
        .get("completed_task_ids")
        .and_then(Value::as_array)
        .map(|ids| {
            ids.iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn required_string<'a>(
    input: &'a Value,
    action: &str,
    field: &str,
) -> Result<&'a str, DispatchError> {
    input
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| DispatchError::action_failed(action, format!("{action} requires `{field}`")))
}

/// Attach `artifacts` to `task_id` as Orbit's system writer: directly on
/// the owner, or across a claimed leaf's host-owned channel, which the
/// owner admits as a system write (the reserved `visual/` prefix refuses
/// every agent and operator route).
fn attach_system_artifacts(
    runtime: &OrbitRuntime,
    task_id: &str,
    artifacts: Vec<TaskArtifact>,
) -> Result<(), OrbitError> {
    if artifacts.is_empty() {
        return Ok(());
    }
    if runtime.worker_invocation().is_some() {
        // One artifact per call keeps each owner request within one image.
        for artifact in artifacts {
            runtime.route_worker_host_tool(
                "orbit.task.artifact.put",
                serde_json::json!({
                    "id": task_id,
                    "artifacts": [{
                        "path": artifact.path,
                        "content": artifact.content,
                        "media_type": artifact.media_type,
                    }],
                }),
            )?;
        }
        return Ok(());
    }
    runtime.update_task_as_system(
        task_id,
        TaskUpdateParams {
            upsert_artifacts: artifacts,
            ..Default::default()
        },
        None,
    )?;
    Ok(())
}

fn system_artifact(path: &str, content: Vec<u8>) -> TaskArtifact {
    TaskArtifact {
        path: path.to_string(),
        content,
        media_type: media_type_for_artifact_path(path).to_string(),
        created_by: None,
    }
}

fn json_artifact<T: serde::Serialize>(path: &str, value: &T) -> Result<TaskArtifact, OrbitError> {
    let mut content = serde_json::to_vec_pretty(value)
        .map_err(|error| OrbitError::Execution(format!("encode {path}: {error}")))?;
    content.push(b'\n');
    Ok(TaskArtifact {
        path: path.to_string(),
        content,
        media_type: "application/json".to_string(),
        created_by: None,
    })
}
