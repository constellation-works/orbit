//! Live visual review resolution (visual-review design §1, §11.3): the
//! confirmed findings `visual_settle` dispositioned go in front of the code
//! reviewer (its manifest's `visual_findings`), its report resolves each
//! (`visual_resolutions`), and `visual_recapture` settles them before the
//! candidate lands. A finding marked fixed counts only when a review commit
//! changed the candidate; then the candidate is recaptured and its checks
//! rerun, and a blocking check finding still present stays unresolved. A
//! must-address finding left unfixed, or disputed without the operator
//! accepting the dispute (`orbit visual accept-dispute`), holds the
//! candidate for the operator. A blocking finding the review gate issued
//! no reviewer for (selective `not_required`, a review-skip tag) cannot be
//! resolved, so it becomes a `high` follow-up instead of a hold.

use std::path::{Path, PathBuf};

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::governance::authorization::agent_context_declared;
use orbit_engine::DispatchError;
use orbit_types::record::OrbitEvent;
use orbit_types::task::{
    TaskComment, TaskHistoryEntry, VISUAL_DISPUTE_ACCEPTED_EVENT, VisualCaptureOutcome,
    VisualCaptureRecord, VisualChecksRecord, VisualDisposition, VisualPass, VisualRecaptureRecord,
    VisualResolution, VisualReviewContract, VisualReviewFinding, VisualReviewMode,
    VisualReviewRecord, VisualVerdictKind, has_verify_mock_ui_tag,
};
use orbit_types::workflow::{
    DeliveryRefusal, REVIEW_MANIFEST_ARTIFACT, REVIEW_REPORT_ARTIFACT, ReviewManifest,
    ReviewReport, ReviewVisualFinding,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::capture::{capture_output, capture_task};
use super::checks::check_task;
use super::contract::MockDigestSource;
use super::{attach_system_artifacts, json_artifact, read_record, required_string, step_task_ids};
use crate::OrbitRuntime;
use crate::application::task::TaskRecordUpdateParams;
use crate::application::task::TaskRecords;

/// The note of a `visual_dispute_accepted` history entry: the finding as
/// the held review recorded it, so a later run's review of the same mock
/// and question finds the acceptance too.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct VisualDisputeAcceptance {
    finding_id: String,
    candidate_sha: String,
    mock: String,
    question: String,
    title: String,
    reason: String,
}

impl VisualDisputeAcceptance {
    fn covers(&self, record: &VisualReviewRecord, finding: &VisualReviewFinding) -> bool {
        (self.candidate_sha == record.candidate_sha && self.finding_id == finding.finding_id)
            || (self.mock == finding.mock
                && self.question == finding.question
                && self.title == finding.title)
    }
}

/// The delivery's live review record of `task_id`, when it is one the
/// reviewer resolves: `mode = "live"` and not recorded only.
fn live_review_record(
    runtime: &OrbitRuntime,
    task_id: &str,
) -> Result<Option<VisualReviewRecord>, OrbitError> {
    let review = read_record::<VisualReviewRecord>(
        runtime,
        task_id,
        &VisualPass::Delivery.review_artifact(),
    )?;
    Ok(review
        .map(|(record, _)| record)
        .filter(|record| record.mode == VisualReviewMode::Live && !record.recorded_only))
}

/// Whether the reviewer resolves `finding`: confirmed, must-address or
/// optional.
fn reviewer_resolves(finding: &VisualReviewFinding) -> bool {
    finding.verdict.verdict == VisualVerdictKind::Confirmed
        && matches!(
            finding.disposition,
            Some(VisualDisposition::MustAddress | VisualDisposition::Optional)
        )
}

fn accepted_disputes(
    runtime: &OrbitRuntime,
    task_id: &str,
) -> Result<Vec<VisualDisputeAcceptance>, OrbitError> {
    Ok(runtime
        .get_task_history(task_id)?
        .iter()
        .filter(|entry| entry.event == VISUAL_DISPUTE_ACCEPTED_EVENT)
        .filter_map(|entry| entry.note.as_deref())
        .filter_map(|note| serde_json::from_str(note).ok())
        .collect())
}

fn candidate_head(workspace: &Path) -> Result<String, OrbitError> {
    let repo = git2::Repository::open(workspace)
        .map_err(|error| OrbitError::Execution(format!("open candidate: {}", error.message())))?;
    repo.head()
        .and_then(|head| head.peel_to_commit())
        .map(|commit| commit.id().to_string())
        .map_err(|error| OrbitError::Execution(format!("candidate head: {}", error.message())))
}

impl OrbitRuntime {
    /// The confirmed findings of `task_id`'s live visual review the code
    /// reviewer resolves; none in shadow, for any other task, or when the
    /// record cannot be read (review admission never fails on it).
    pub(crate) fn visual_findings_for_review(
        &self,
        task_id: &str,
    ) -> Result<Vec<ReviewVisualFinding>, OrbitError> {
        let Ok(Some(record)) = live_review_record(self, task_id) else {
            return Ok(Vec::new());
        };
        Ok(record
            .findings
            .iter()
            .filter(|finding| reviewer_resolves(finding))
            .filter(|finding| {
                !matches!(finding.resolution, Some(VisualResolution::DisputeAccepted))
            })
            .map(|finding| {
                let mut images: Vec<String> = finding
                    .evidence
                    .iter()
                    .map(|evidence| evidence.image.clone())
                    .collect();
                images.dedup();
                ReviewVisualFinding {
                    finding_id: finding.finding_id.clone(),
                    mock: finding.mock.clone(),
                    question: finding.question.clone(),
                    title: finding.title.clone(),
                    detail: finding.detail.clone(),
                    severity: finding.severity_of_record().to_string(),
                    must_address: finding.must_address(),
                    images,
                }
            })
            .collect())
    }

    /// The operator's acceptance of the code reviewer's dispute of a
    /// blocking visual finding: a `visual_dispute_accepted` history entry
    /// and comment, and the finding's resolution in `visual/review.json`.
    pub(crate) fn visual_accept_dispute(
        &self,
        task_id: &str,
        finding_id: &str,
        reason: &str,
    ) -> Result<Value, OrbitError> {
        if agent_context_declared() || self.worker_invocation().is_some() {
            return Err(OrbitError::CapabilityDenied(
                "managed runs cannot accept a visual finding's dispute; an operator does".into(),
            ));
        }
        let reason = reason.trim();
        if reason.is_empty() {
            return Err(OrbitError::InvalidInput(
                "`reason` must say why the dispute is accepted".into(),
            ));
        }
        self.get_task(task_id)?;
        self.ensure_coordination_task_write_permitted()?;
        let Some(mut record) = live_review_record(self, task_id)? else {
            return Err(OrbitError::InvalidInput(format!(
                "task '{task_id}' has no live visual review record; only a live review's \
                 disputed blocking finding can be accepted"
            )));
        };
        let Some(finding) = record
            .findings
            .iter_mut()
            .find(|finding| finding.finding_id == finding_id)
        else {
            return Err(OrbitError::InvalidInput(format!(
                "task '{task_id}' has no visual finding '{finding_id}'; `orbit task show` lists \
                 its visual review findings"
            )));
        };
        if !finding.must_address() || finding.resolution != Some(VisualResolution::Disputed) {
            return Err(OrbitError::InvalidInput(format!(
                "visual finding '{finding_id}' is not a disputed blocking finding (disposition {}, \
                 resolution {}); only a dispute the code reviewer recorded can be accepted",
                finding
                    .disposition
                    .map_or("none", VisualDisposition::as_str),
                finding.resolution.map_or("none", VisualResolution::as_str),
            )));
        }
        let actor = self.actor().resolve_write_label(None, None)?;
        let acceptance = VisualDisputeAcceptance {
            finding_id: finding.finding_id.clone(),
            candidate_sha: record.candidate_sha.clone(),
            mock: finding.mock.clone(),
            question: finding.question.clone(),
            title: finding.title.clone(),
            reason: reason.to_string(),
        };
        let note = serde_json::to_string(&acceptance)
            .map_err(|error| OrbitError::Execution(format!("encode acceptance: {error}")))?;
        let now = Utc::now();
        self.stores()
            .tasks()
            .with_task_write_lock(task_id, &mut || {
                self.with_mutation(|| {
                    let updated = self.stores().task_records().update(
                        task_id,
                        TaskRecordUpdateParams {
                            actor: actor.clone(),
                            append_history: vec![TaskHistoryEntry {
                                at: now,
                                by: actor.clone(),
                                event: VISUAL_DISPUTE_ACCEPTED_EVENT.to_string(),
                                note: Some(note.clone()),
                                from_status: None,
                                to_status: None,
                                operation_id: None,
                                decision: None,
                            }],
                            append_comments: vec![TaskComment {
                                at: now,
                                by: actor.clone(),
                                message: format!(
                                    "visual review: accepted the dispute of finding {finding_id} \
                                     ({}): {reason}",
                                    acceptance.title
                                ),
                                operation_id: None,
                            }],
                            ..Default::default()
                        },
                    )?;
                    Ok((
                        updated,
                        OrbitEvent::TaskUpdated {
                            id: task_id.to_string(),
                        },
                    ))
                })?;
                Ok(())
            })?;
        finding.resolution = Some(VisualResolution::DisputeAccepted);
        finding.resolution_reason = Some(format!("accepted by {actor}: {reason}"));
        attach_system_artifacts(
            self,
            task_id,
            vec![json_artifact(
                &VisualPass::Delivery.review_artifact(),
                &record,
            )?],
        )?;
        Ok(json!({
            "task_id": task_id,
            "finding_id": finding_id,
            "resolution": VisualResolution::DisputeAccepted.as_str(),
        }))
    }
}

/// `visual_recapture`: settle the code reviewer's resolutions of each
/// task's live visual findings before the candidate lands, recapturing a
/// candidate a review commit changed. Refuses with
/// `delivery_visual_review_unresolved`, holding the candidate, while a
/// must-address finding is unresolved.
pub(crate) fn visual_recapture_step(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
) -> Result<Value, DispatchError> {
    let workspace = PathBuf::from(required_string(input, action, "workspace_path")?);
    let run_id = input
        .get("job_run_id")
        .and_then(Value::as_str)
        .filter(|run| !run.trim().is_empty())
        .map(str::to_string)
        .or_else(|| {
            runtime
                .worker_invocation()
                .map(|binding| binding.bound_run_id.clone())
        })
        .unwrap_or_else(|| "run".to_string());
    let mut resolved = Vec::new();
    let mut unresolved = Vec::new();
    for task_id in step_task_ids(input) {
        let entry = resolve_task(runtime, &task_id, &workspace, &run_id)
            .map_err(|error| DispatchError::action_failed(action, error.to_string()))?;
        if let Some((entry, held)) = entry {
            unresolved.extend(held);
            resolved.push(entry);
        }
    }
    if !unresolved.is_empty() {
        return Err(DispatchError::DeliveryRefused {
            action: action.to_string(),
            refusal: DeliveryRefusal::VisualReviewUnresolved,
            message: format!(
                "confirmed visual findings at or above `visual_review.block_at` are unresolved, \
                 so the candidate is held for the operator: {}. Fix them, or accept a disputed \
                 one with `orbit visual accept-dispute`",
                unresolved.join("; ")
            ),
        });
    }
    Ok(json!({
        "applies": !resolved.is_empty(),
        "resolved": resolved,
    }))
}

/// One task's resolution entry and the unresolved blocking findings it
/// holds on; `None` when the task has no live review to resolve.
fn resolve_task(
    runtime: &OrbitRuntime,
    task_id: &str,
    workspace: &Path,
    run_id: &str,
) -> Result<Option<(Value, Vec<String>)>, OrbitError> {
    let task = runtime.get_task(task_id)?;
    if runtime.visual_review_settings().mode != VisualReviewMode::Live
        || !has_verify_mock_ui_tag(&task.tags)
    {
        return Ok(None);
    }
    let missing = |reason: &str| {
        Some((
            json!({"task_id":task_id,"unresolved":1}),
            vec![format!("{task_id}: {reason}")],
        ))
    };
    let Some(mut record) = live_review_record(runtime, task_id)? else {
        return Ok(missing(
            "the current delivery has no live visual review evidence",
        ));
    };
    let contract = match runtime.visual_review_contract_for(&task, MockDigestSource::Frozen) {
        Ok(Some(contract)) => contract,
        _ => return Ok(missing("the frozen visual contract cannot be established")),
    };
    for mock in &contract.mocks {
        let stored = runtime.get_task_artifact(task_id, &mock.artifact)?;
        if stored.as_ref().is_none_or(|artifact| {
            orbit_types::task::mock_sha256_hex(&artifact.content) != mock.sha256
        }) {
            return Ok(missing(
                "the mock differs from the frozen delivery contract",
            ));
        }
    }
    if contract.reviewer && (record.outcome != "reviewed" || !record.not_established.is_empty()) {
        return Ok(missing("the comparison review is not established"));
    }
    let head = candidate_head(workspace)?;
    let changed = head != record.candidate_sha;
    let resolutions = reviewer_resolutions(runtime, task_id)?;
    let issued = reviewer_issued_findings(runtime, task_id, &record)?;
    let acceptances = accepted_disputes(runtime, task_id)?;
    let snapshot = record.clone();
    for finding in record.findings.iter_mut().filter(|f| reviewer_resolves(f)) {
        if finding.resolution == Some(VisualResolution::DisputeAccepted) {
            continue;
        }
        if finding.must_address()
            && let Some(acceptance) = acceptances
                .iter()
                .find(|acceptance| acceptance.covers(&snapshot, finding))
        {
            finding.resolution = Some(VisualResolution::DisputeAccepted);
            finding.resolution_reason = Some(acceptance.reason.clone());
            continue;
        }
        if finding.must_address() && !issued.contains(&finding.finding_id) {
            // Settle classified from the captured admission; the review gate
            // then ran no reviewer (selective `not_required`, a review-skip
            // tag), so nothing can resolve or dispute this finding. It is
            // filed as a `high` follow-up rather than held (§11.3).
            finding.disposition = Some(VisualDisposition::FollowUpHigh);
            finding.resolution = None;
            finding.resolution_reason = None;
            continue;
        }
        let claimed = resolutions
            .iter()
            .find(|resolution| resolution.finding_id == finding.finding_id);
        let (resolution, reason) = match claimed.map(|claim| claim.resolution.trim()) {
            Some("fixed") if changed => (VisualResolution::Fixed, None),
            Some("fixed") => (
                VisualResolution::Unfixed,
                Some("marked fixed, but no review commit changed the candidate".to_string()),
            ),
            Some("disputed") => (
                VisualResolution::Disputed,
                claimed.and_then(|claim| claim.reason.clone()),
            ),
            Some(_) => (
                VisualResolution::Unfixed,
                claimed.and_then(|claim| claim.reason.clone()),
            ),
            None => (
                VisualResolution::Unfixed,
                Some("the code reviewer did not resolve it".to_string()),
            ),
        };
        finding.resolution = Some(resolution);
        finding.resolution_reason = reason;
    }
    if changed {
        recapture(runtime, task_id, workspace, run_id, &head, &mut record)?;
    }
    attach_system_artifacts(
        runtime,
        task_id,
        vec![json_artifact(
            &VisualPass::Delivery.review_artifact(),
            &record,
        )?],
    )?;
    let mut held: Vec<String> = record
        .unresolved_blocking()
        .iter()
        .map(|finding| {
            format!(
                "{task_id} {} ({}, {}): {}",
                finding.finding_id,
                finding.severity_of_record(),
                finding
                    .resolution
                    .map_or("unresolved", VisualResolution::as_str),
                finding.title
            )
        })
        .collect();
    let pass = if changed {
        VisualPass::Recapture
    } else {
        VisualPass::Delivery
    };
    let established =
        read_record::<VisualCaptureRecord>(runtime, task_id, &pass.capture_artifact())?
            .is_some_and(|(capture, _)| capture_matches_final_contract(&capture, &contract, &head));
    if !established
        || (changed
            && record.recapture.as_ref().is_none_or(|recapture| {
                recapture.candidate_sha != head || recapture.outcome != "passed"
            }))
    {
        held.push(format!("{task_id}: visual evidence is missing, incomplete or stale for the final candidate {head}"));
    }
    let (blocking, optional, _) = record.disposition_counts();
    Ok(Some((
        json!({
            "task_id": task_id,
            "candidate_sha": head,
            "candidate_changed": changed,
            "blocking": blocking,
            "unresolved": held.len(),
            "optional": optional,
            "recapture": record.recapture,
        }),
        held,
    )))
}

/// The reviewer's resolutions, read only from a report for the manifest
/// this delivery's reviewer was last issued.
fn reviewer_resolutions(
    runtime: &OrbitRuntime,
    task_id: &str,
) -> Result<Vec<orbit_types::workflow::ReviewVisualResolution>, OrbitError> {
    let manifest = runtime
        .get_task_artifact(task_id, REVIEW_MANIFEST_ARTIFACT)?
        .and_then(|artifact| serde_json::from_slice::<ReviewManifest>(&artifact.content).ok());
    let report = runtime
        .get_task_artifact(task_id, REVIEW_REPORT_ARTIFACT)?
        .and_then(|artifact| ReviewReport::parse(&artifact.content).ok());
    Ok(match (manifest, report) {
        (Some(manifest), Some(report))
            if report.attempt_id == manifest.attempt_id && !manifest.visual_findings.is_empty() =>
        {
            report.visual_resolutions
        }
        _ => Vec::new(),
    })
}

/// The ids of `record`'s findings a before-landing reviewer was issued: the
/// `visual_findings` of a manifest the review gate wrote after settle
/// recorded `record`. A delivery whose gate ran no reviewer has none.
fn reviewer_issued_findings(
    runtime: &OrbitRuntime,
    task_id: &str,
    record: &VisualReviewRecord,
) -> Result<Vec<String>, OrbitError> {
    let Some(manifest) = runtime
        .get_task_artifact(task_id, REVIEW_MANIFEST_ARTIFACT)?
        .and_then(|artifact| serde_json::from_slice::<ReviewManifest>(&artifact.content).ok())
    else {
        return Ok(Vec::new());
    };
    let settled_at = chrono::DateTime::parse_from_rfc3339(&record.recorded_at).ok();
    if settled_at.is_some_and(|settled_at| manifest.issued_at < settled_at) {
        return Ok(Vec::new());
    }
    Ok(manifest
        .visual_findings
        .into_iter()
        .map(|finding| finding.finding_id)
        .collect())
}

/// Recapture the changed candidate and rerun its checks only (§1),
/// recording which fixed check findings are still present.
fn recapture(
    runtime: &OrbitRuntime,
    task_id: &str,
    workspace: &Path,
    run_id: &str,
    head: &str,
    record: &mut VisualReviewRecord,
) -> Result<(), OrbitError> {
    let pass = VisualPass::Recapture;
    let fixed_findings = |record: &VisualReviewRecord| -> Vec<String> {
        record
            .findings
            .iter()
            .filter(|finding| finding.resolution == Some(VisualResolution::Fixed))
            .map(|finding| finding.finding_id.clone())
            .collect()
    };
    for finding in &mut record.findings {
        finding.recaptured.clear();
    }
    let Some(capture) = capture_task(runtime, task_id, workspace, run_id, pass)? else {
        record.recapture = Some(VisualRecaptureRecord {
            candidate_sha: head.to_string(),
            outcome: "not_established".to_string(),
            still_failing: fixed_findings(record),
        });
        return Ok(());
    };
    let established = capture.record.outcome == VisualCaptureOutcome::Captured
        && capture.record.candidate_sha == head
        && capture.record.not_established.is_empty()
        && capture.record.over_budget.is_empty()
        && !capture.record.mocks.is_empty()
        && capture.record.mocks.iter().all(|mock| {
            mock.error.is_none()
                && mock
                    .capture
                    .as_ref()
                    .is_some_and(|image| image.commit == head)
        });
    let images: Vec<(String, Vec<String>)> = capture
        .record
        .mocks
        .iter()
        .filter_map(|mock| {
            mock.capture.as_ref().map(|image| {
                (
                    mock.artifact.clone(),
                    vec![image.path.clone(), image.side_by_side.clone()],
                )
            })
        })
        .collect();
    let output = capture_output(std::slice::from_ref(&capture));
    if let Some(entry) = output
        .get("captures")
        .and_then(Value::as_array)
        .and_then(|entries| entries.first())
    {
        check_task(runtime, entry, pass)?;
    }
    let checks = read_record::<VisualChecksRecord>(runtime, task_id, &pass.checks_artifact())?
        .map(|(checks, _)| checks)
        .filter(|checks| checks.candidate_sha == head);
    let still_failing: Vec<String> = match (&checks, established) {
        (Some(checks), true) => record
            .findings
            .iter()
            .filter(|finding| {
                finding.resolution == Some(VisualResolution::Fixed) && finding.source == "check"
            })
            .filter(|finding| {
                checks.findings.iter().any(|check| {
                    check.check == finding.question
                        && check.mock.as_deref() == Some(finding.mock.as_str())
                })
            })
            .map(|finding| finding.finding_id.clone())
            .collect(),
        _ => fixed_findings(record),
    };
    for finding in &mut record.findings {
        if finding.resolution == Some(VisualResolution::Fixed) {
            // Never retain images from a previous recapture when this one
            // failed to render the finding's own mock.
            finding.recaptured.clear();
            if let Some((_, recaptured)) = images.iter().find(|(mock, _)| *mock == finding.mock) {
                finding.recaptured.clone_from(recaptured);
            }
        }
    }
    let missing_fixed_mock = record.findings.iter().any(|finding| {
        finding.must_address()
            && finding.resolution == Some(VisualResolution::Fixed)
            && !capture.record.mocks.iter().any(|mock| {
                mock.artifact == finding.mock
                    && mock.error.is_none()
                    && mock
                        .capture
                        .as_ref()
                        .is_some_and(|image| image.commit == head)
            })
    });
    let blocking_check = checks.as_ref().is_some_and(|checks| {
        checks.findings.iter().any(|finding| {
            record
                .block_at
                .unwrap_or(orbit_types::task::VisualThreshold::High)
                .admits(&finding.severity)
        })
    });
    let outcome = if !established || checks.is_none() || missing_fixed_mock {
        "not_established"
    } else if still_failing.is_empty() && !blocking_check {
        "passed"
    } else {
        "incomplete"
    };
    record.recapture = Some(VisualRecaptureRecord {
        candidate_sha: head.to_string(),
        outcome: outcome.to_string(),
        still_failing,
    });
    Ok(())
}

/// Every frozen mock must have rendered the exact candidate before it can land.
fn capture_matches_final_contract(
    capture: &VisualCaptureRecord,
    contract: &VisualReviewContract,
    head: &str,
) -> bool {
    capture.task_id == contract.task_id
        && capture.candidate_sha == head
        && capture.outcome == VisualCaptureOutcome::Captured
        && capture.not_established.is_empty()
        && capture.over_budget.is_empty()
        && !contract.mocks.is_empty()
        && capture.mocks.len() == contract.mocks.len()
        && contract.mocks.iter().all(|expected| {
            capture.mocks.iter().any(|mock| {
                mock.artifact == expected.artifact
                    && mock.sha256 == expected.sha256
                    && mock.error.is_none()
                    && mock
                        .capture
                        .as_ref()
                        .is_some_and(|image| image.commit == head)
            })
        })
}
