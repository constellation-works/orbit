//! The delivery's visual review phase (visual-review design §4, §6, §7):
//! for a `verify-mock-ui` task whose `visual_review.mode` is not `off`,
//! capture the final candidate once per mock in a detached checkout
//! (`visual_capture`), record the deterministic checks (`visual_checks`)
//! and settle the capture against the candidate and the frozen mocks
//! (`visual_settle`). Everything is written by the system writer under the
//! reserved `visual/` prefix. In this phase nothing blocks and nothing is
//! filed; only `visual_review.require_established` holds a delivery whose
//! capture is not established.
//!
//! A claimed leaf captures against the visual review contract its owner
//! captured at admission, and the owner checks what the leaf returns before
//! it accepts the handoff (`visual_capture_mismatch`).

mod capture;
mod checks;
mod contract;
mod handoff;
mod settle;

pub(crate) use capture::visual_capture_step;
pub(crate) use checks::visual_checks_step;
pub(crate) use settle::visual_settle_step;

use orbit_common::OrbitError;
use orbit_engine::DispatchError;
use orbit_types::task::{TaskArtifact, media_type_for_artifact_path};
use serde_json::Value;

use crate::OrbitRuntime;
use crate::application::task::TaskUpdateParams;

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

/// A stored `visual/*.json` record, parsed; `None` when it is absent.
fn read_record<T: serde::de::DeserializeOwned>(
    runtime: &OrbitRuntime,
    task_id: &str,
    path: &str,
) -> Result<Option<(T, Vec<u8>)>, OrbitError> {
    let Some(artifact) = runtime.get_task_artifact(task_id, path)? else {
        return Ok(None);
    };
    let record = serde_json::from_slice(&artifact.content)
        .map_err(|error| OrbitError::Execution(format!("decode {path} of {task_id}: {error}")))?;
    Ok(Some((record, artifact.content)))
}

impl OrbitRuntime {
    /// A `verify-mock-ui` task's last capture of record for task show:
    /// its summary line (never a pass when nothing was captured), outcome,
    /// `not_established` reasons, check findings and settlement. `None` for
    /// any other task, or one never captured.
    pub(crate) fn task_visual_review_json(
        &self,
        task: &orbit_types::task::Task,
    ) -> Result<Option<Value>, OrbitError> {
        use orbit_types::task::{
            VISUAL_CAPTURE_ARTIFACT, VISUAL_CHECKS_ARTIFACT, VISUAL_SETTLE_ARTIFACT,
            VisualCaptureRecord, VisualChecksRecord, VisualSettleRecord, has_verify_mock_ui_tag,
            visual_review_summary,
        };
        if !has_verify_mock_ui_tag(&task.tags) {
            return Ok(None);
        }
        let Some(capture) = self.get_task_artifact(&task.id, VISUAL_CAPTURE_ARTIFACT)? else {
            return Ok(None);
        };
        let Ok(record) = VisualCaptureRecord::parse(&capture.content) else {
            return Ok(Some(serde_json::json!({
                "summary": "capture record unreadable",
            })));
        };
        let checks = read_record::<VisualChecksRecord>(self, &task.id, VISUAL_CHECKS_ARTIFACT)
            .ok()
            .flatten()
            .map(|(checks, _)| checks)
            .filter(|checks| checks.candidate_sha == record.candidate_sha);
        let settle = read_record::<VisualSettleRecord>(self, &task.id, VISUAL_SETTLE_ARTIFACT)
            .ok()
            .flatten()
            .map(|(settle, _)| settle)
            .filter(|settle| settle.candidate_sha == record.candidate_sha);
        Ok(Some(serde_json::json!({
            "summary": visual_review_summary(&record, checks.as_ref(), settle.as_ref()),
            "mode": record.mode.as_str(),
            "outcome": record.outcome,
            "candidate_sha": record.candidate_sha,
            "not_established": record.not_established,
            "findings": checks.as_ref().map(|checks| &checks.findings),
            "settle": settle.as_ref().map(|settle| serde_json::json!({
                "outcome": settle.outcome,
                "refusals": settle.refusals,
                "held": settle.held,
            })),
        })))
    }
}
