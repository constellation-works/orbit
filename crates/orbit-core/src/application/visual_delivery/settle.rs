//! `visual_settle`: hold the capture of record to the candidate and the
//! frozen mocks (§4.4). A capture whose commit is not the delivery's head
//! is `stale_capture`; a mock whose bytes differ from the digest frozen at
//! admission is `mock_changed`. Both are recorded in `visual/settle.json`
//! and in the one-line summary run show, task show and the PR body print.
//! In this phase they block nothing. `visual_review.require_established`
//! holds a delivery whose capture is not established.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_engine::DispatchError;
use orbit_types::task::{
    VISUAL_CAPTURE_ARTIFACT, VISUAL_CHECKS_ARTIFACT, VISUAL_SETTLE_ARTIFACT, VisualCaptureOutcome,
    VisualCaptureRecord, VisualChecksRecord, VisualReviewContract, VisualSettleOutcome,
    VisualSettleRecord, VisualSettleRefusal, mock_sha256_hex, visual_review_summary,
};
use orbit_types::workflow::DeliveryRefusal;
use serde_json::{Value, json};

use super::contract::MockDigestSource;
use super::{attach_system_artifacts, json_artifact, read_record, required_string};
use crate::OrbitRuntime;

pub(crate) fn visual_settle_step(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
) -> Result<Value, DispatchError> {
    let workspace = Path::new(required_string(input, action, "workspace_path")?);
    let captures = input
        .get("capture")
        .and_then(|capture| capture.get("captures"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut settled = Vec::new();
    let mut held = Vec::new();
    for capture in &captures {
        let (entry, hold) = settle_task(runtime, workspace, capture)
            .map_err(|error| DispatchError::action_failed(action, error.to_string()))?;
        if let Some(hold) = hold {
            held.push(hold);
        }
        settled.push(entry);
    }
    if !held.is_empty() {
        return Err(DispatchError::DeliveryRefused {
            action: action.to_string(),
            refusal: DeliveryRefusal::VisualNotEstablished,
            message: format!(
                "visual review is not established and `visual_review.require_established` holds \
                 the delivery: {}",
                held.join("; ")
            ),
        });
    }
    let summary = settled
        .iter()
        .filter_map(|entry| entry.get("summary").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("; ");
    Ok(json!({
        "applies": !settled.is_empty(),
        "settled": settled,
        "summary": summary,
    }))
}

fn settle_task(
    runtime: &OrbitRuntime,
    workspace: &Path,
    capture: &Value,
) -> Result<(Value, Option<String>), OrbitError> {
    let task_id = capture
        .get("task_id")
        .and_then(Value::as_str)
        .ok_or_else(|| OrbitError::InvalidInput("capture entry names no task_id".into()))?;
    let (record, _) =
        read_record::<VisualCaptureRecord>(runtime, task_id, VISUAL_CAPTURE_ARTIFACT)?.ok_or_else(
            || OrbitError::Execution(format!("{task_id} has no {VISUAL_CAPTURE_ARTIFACT}")),
        )?;
    let checks = read_record::<VisualChecksRecord>(runtime, task_id, VISUAL_CHECKS_ARTIFACT)?
        .map(|(checks, _)| checks)
        .filter(|checks| checks.candidate_sha == record.candidate_sha);
    let head = crate::application::tag_instructions::commit_at(workspace, "HEAD")
        .map_err(|reason| OrbitError::Execution(format!("candidate head: {reason}")))?;
    let mut refusals = Vec::new();
    if record.candidate_sha != head {
        refusals.push(VisualSettleRefusal {
            code: "stale_capture".to_string(),
            mock: None,
            detail: format!(
                "the capture is of {}, the delivery's head is {head}",
                record.candidate_sha
            ),
        });
    }
    for mock in &record.mocks {
        if let Some(image) = &mock.capture
            && image.commit != head
        {
            refusals.push(VisualSettleRefusal {
                code: "stale_capture".to_string(),
                mock: Some(mock.artifact.clone()),
                detail: format!("the capture shows {}, not {head}", image.commit),
            });
        }
    }
    let contract = match settle_contract(runtime, task_id) {
        Ok(contract) => contract,
        Err(error) => {
            refusals.push(VisualSettleRefusal {
                code: "mock_changed".to_string(),
                mock: None,
                detail: error.to_string(),
            });
            None
        }
    };
    if let Some(contract) = &contract {
        for frozen in &contract.mocks {
            let captured = record
                .mocks
                .iter()
                .find(|mock| mock.artifact == frozen.artifact);
            let stored = runtime
                .get_task_artifact(task_id, &frozen.artifact)?
                .map(|artifact| mock_sha256_hex(&artifact.content));
            let captured_differs = captured.is_some_and(|mock| mock.sha256 != frozen.sha256);
            if captured_differs || stored.as_deref() != Some(frozen.sha256.as_str()) {
                refusals.push(VisualSettleRefusal {
                    code: "mock_changed".to_string(),
                    mock: Some(frozen.artifact.clone()),
                    detail: "the mock's bytes differ from the digest frozen when the delivery \
                             was admitted"
                        .to_string(),
                });
            }
        }
    }
    let outcome = if !refusals.is_empty() {
        VisualSettleOutcome::Refused
    } else if record.outcome == VisualCaptureOutcome::NotEstablished {
        VisualSettleOutcome::NotEstablished
    } else {
        VisualSettleOutcome::Established
    };
    // The owner reads its current setting, so clearing it and resuming the
    // held run releases the delivery; a leaf holds to its claim's contract.
    let require_established = if runtime.worker_invocation().is_some() {
        contract
            .as_ref()
            .map(|contract| contract.require_established)
            .or_else(|| capture.get("require_established").and_then(Value::as_bool))
            .unwrap_or(false)
    } else {
        runtime.visual_review_settings().require_established
    };
    let held = require_established && outcome != VisualSettleOutcome::Established;
    let settle = VisualSettleRecord {
        schema_version: 1,
        candidate_sha: head,
        outcome,
        refusals,
        not_established: record.not_established.clone(),
        held,
    };
    attach_system_artifacts(
        runtime,
        task_id,
        vec![json_artifact(VISUAL_SETTLE_ARTIFACT, &settle)?],
    )?;
    let summary = visual_review_summary(&record, checks.as_ref(), Some(&settle));
    let hold = held.then(|| format!("{task_id}: {summary}"));
    Ok((
        json!({
            "task_id": task_id,
            "outcome": match settle.outcome {
                VisualSettleOutcome::Established => "established",
                VisualSettleOutcome::NotEstablished => "not_established",
                VisualSettleOutcome::Refused => "refused",
            },
            "held": held,
            "settle_artifact": VISUAL_SETTLE_ARTIFACT,
            "summary": summary,
        }),
        hold,
    ))
}

/// The contract the capture ran: the claim's receipt on a leaf, the frozen
/// digests and current settings on the owner.
fn settle_contract(
    runtime: &OrbitRuntime,
    task_id: &str,
) -> Result<Option<VisualReviewContract>, OrbitError> {
    if runtime.worker_invocation().is_some() {
        return runtime.claimed_visual_review(task_id);
    }
    let task = runtime.get_task(task_id)?;
    runtime
        .visual_review_contract_for(&task, MockDigestSource::Frozen)
        .map_err(|detail| {
            OrbitError::Execution(format!(
                "visual review contract for {task_id} cannot be read: {detail}"
            ))
        })
}
