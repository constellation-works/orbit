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
    VISUAL_CAPTURE_ARTIFACT, VISUAL_CHECKS_ARTIFACT, VISUAL_REVIEW_ARTIFACT, VISUAL_REVIEW_PURPOSE,
    VISUAL_SETTLE_ARTIFACT, VisualCaptureOutcome, VisualCaptureRecord, VisualChecksRecord,
    VisualDeviationRecord, VisualFindingVerdict, VisualReviewBudgetRecord, VisualReviewContract,
    VisualReviewFinding, VisualReviewRecord, VisualSettleOutcome, VisualSettleRecord,
    VisualSettleRefusal, VisualVerdictKind, mock_sha256_hex, visual_review_summary_with_review,
};
use orbit_types::workflow::DeliveryRefusal;
use serde_json::{Value, json};

use super::contract::MockDigestSource;
use super::verify::{catalog_of, collect_result_objects, visual_verdicts_by_finding};
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
        let (entry, hold) = settle_task(runtime, workspace, capture, input)
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
    input: &Value,
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
    // The comparison review of record (§11): findings with their
    // verdicts and verified severities. Shadow only — recorded under
    // purpose `visual_review`, never filed, never blocking and never
    // given to the code reviewer.
    let review = review_record(workspace, &record, task_id, input)?;
    if let Some(review) = &review {
        attach_system_artifacts(
            runtime,
            task_id,
            vec![json_artifact(VISUAL_REVIEW_ARTIFACT, review)?],
        )?;
    }
    let summary =
        visual_review_summary_with_review(&record, checks.as_ref(), Some(&settle), review.as_ref());
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
            "review_artifact": review.is_some().then_some(VISUAL_REVIEW_ARTIFACT),
            "summary": summary,
        }),
        hold,
    ))
}

/// `visual/review.json` for one task, from the verify-prepare records
/// and the verifier fan-outs this step was given. `None` when the
/// pipeline ran no review preparation for it (an older job shape).
fn review_record(
    workspace: &Path,
    capture: &VisualCaptureRecord,
    task_id: &str,
    input: &Value,
) -> Result<Option<VisualReviewRecord>, OrbitError> {
    let Some(prepared) = input.get("verified") else {
        return Ok(None);
    };
    let Some(record) = prepared
        .get("records")
        .and_then(Value::as_array)
        .and_then(|records| {
            records
                .iter()
                .find(|record| record.get("task_id").and_then(Value::as_str) == Some(task_id))
        })
    else {
        return Ok(None);
    };
    let catalog = catalog_of(record);
    let initial = input.get("verify_results").cloned().unwrap_or(Value::Null);
    let escalated = input
        .get("escalation_results")
        .cloned()
        .unwrap_or(Value::Null);
    let initial_verdicts = visual_verdicts_by_finding(&initial, &catalog, workspace);
    let escalation_verdicts = visual_verdicts_by_finding(&escalated, &catalog, workspace);
    let initial_crews = partition_crews(prepared);
    let escalation_crews = input
        .get("escalation_prepared")
        .map(partition_crews)
        .unwrap_or_default();
    let mut findings: Vec<VisualReviewFinding> = record
        .get("findings")
        .and_then(Value::as_array)
        .map(|findings| {
            findings
                .iter()
                .filter_map(|finding| serde_json::from_value(finding.clone()).ok())
                .collect()
        })
        .unwrap_or_default();
    for finding in &mut findings {
        let initial = initial_verdicts.get(&finding.finding_id);
        let escalation = escalation_verdicts.get(&finding.finding_id);
        // One escalation on low confidence; a low-confidence verdict
        // that no escalation decided leaves the finding unverified.
        let (deciding, crew, escalated) = match (initial, escalation) {
            (Some(verdict), _) if verdict.confidence != "low" => {
                (Some(verdict), initial_crews.get(&finding.finding_id), false)
            }
            (_, Some(verdict)) if verdict.confidence != "low" => (
                Some(verdict),
                escalation_crews.get(&finding.finding_id),
                true,
            ),
            _ => (None, None, escalation.is_some()),
        };
        if let Some(verdict) = deciding {
            if verdict.verdict == VisualVerdictKind::Confirmed {
                finding.verified_severity.clone_from(&verdict.severity);
                finding.severity_source = Some("verified".to_string());
            }
            finding.verdict = VisualFindingVerdict {
                verdict: verdict.verdict,
                crew: crew.cloned(),
                confidence: Some(verdict.confidence.clone()),
                severity: verdict.severity.clone(),
                escalated,
                reasoning: verdict.reasoning.clone(),
                deviation_rejected: verdict.deviation_rejected,
            };
        } else if initial.is_some() || escalation.is_some() {
            finding.verdict = VisualFindingVerdict {
                verdict: VisualVerdictKind::Unverified,
                crew: None,
                confidence: Some("low".to_string()),
                severity: None,
                escalated,
                reasoning: "the only verdicts returned were low confidence".to_string(),
                deviation_rejected: false,
            };
        }
    }
    let deviations = deviation_records(record, &findings);
    let budget: VisualReviewBudgetRecord = record
        .get("budget")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|error| OrbitError::Execution(format!("visual budget record: {error}")))?
        .unwrap_or(VisualReviewBudgetRecord {
            max_mocks: 0,
            max_images: 0,
            max_image_tokens: 0,
            max_findings: 0,
            images_shown: 0,
            estimated_image_tokens: 0,
            shown: Vec::new(),
            not_shown: Vec::new(),
        });
    let mut verifier_crews: Vec<String> = findings
        .iter()
        .filter_map(|finding| finding.verdict.crew.clone())
        .collect();
    verifier_crews.sort();
    verifier_crews.dedup();
    Ok(Some(VisualReviewRecord {
        schema_version: 1,
        purpose: VISUAL_REVIEW_PURPOSE.to_string(),
        mode: capture.mode,
        task_id: task_id.to_string(),
        candidate_sha: capture.candidate_sha.clone(),
        recorded_at: chrono::Utc::now().to_rfc3339(),
        outcome: record
            .get("outcome")
            .and_then(Value::as_str)
            .unwrap_or("reviewed")
            .to_string(),
        not_established: record
            .get("not_established")
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or_default(),
        reviewer_crew: record
            .get("reviewer_crew")
            .and_then(Value::as_str)
            .map(str::to_string),
        verifier_crews,
        budget,
        findings,
        invalid_evidence_dropped: u32::try_from(
            record
                .get("invalid_evidence_dropped")
                .and_then(Value::as_u64)
                .unwrap_or(0),
        )
        .unwrap_or(u32::MAX),
        over_max_findings_dropped: u32::try_from(
            record
                .get("over_max_findings_dropped")
                .and_then(Value::as_u64)
                .unwrap_or(0),
        )
        .unwrap_or(u32::MAX),
        deviations,
        recorded_only: true,
    }))
}

/// The crew each finding's partition named, by finding id.
fn partition_crews(prepared: &Value) -> std::collections::BTreeMap<String, String> {
    let mut crews = std::collections::BTreeMap::new();
    for partition in collect_result_objects(prepared.get("partitions"), "findings") {
        let Some(crew) = partition.get("crew").and_then(Value::as_str) else {
            continue;
        };
        if let Some(findings) = partition.get("findings").and_then(Value::as_array) {
            for finding in findings {
                if let Some(id) = finding.get("finding_id").and_then(Value::as_str) {
                    crews.insert(id.to_string(), crew.to_string());
                }
            }
        }
    }
    crews
}

/// Each declared deviation with the verdict its touching findings
/// imply (§2.5): `rejected` when a CONFIRMED finding rejected its
/// basis, `accepted` when a touching finding was REJECTED, else
/// `unjudged`.
fn deviation_records(
    record: &Value,
    findings: &[VisualReviewFinding],
) -> Vec<VisualDeviationRecord> {
    record
        .get("deviations")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|deviation| {
            let mock = deviation.get("mock")?.as_str()?.to_string();
            let touching: Vec<&VisualReviewFinding> = findings
                .iter()
                .filter(|finding| finding.mock == mock)
                .collect();
            let status = if touching.iter().any(|finding| {
                finding.verdict.deviation_rejected
                    && finding.verdict.verdict == VisualVerdictKind::Confirmed
            }) {
                "rejected"
            } else if touching
                .iter()
                .any(|finding| finding.verdict.verdict == VisualVerdictKind::Rejected)
            {
                "accepted"
            } else {
                "unjudged"
            };
            Some(VisualDeviationRecord {
                mock,
                element: deviation
                    .get("element")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                region: deviation.get("region").and_then(super::verify::region_of),
                reason: deviation
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                basis: deviation
                    .get("basis")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                status: status.to_string(),
            })
        })
        .collect()
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
