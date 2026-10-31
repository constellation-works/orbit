//! `visual_landed` (visual-review design §11.3, §12.1): after a
//! `verify-mock-ui` delivery lands in `visual_review.mode = "live"`, file
//! each confirmed finding the delivery left as a proposed follow-up (an
//! optional fix not made, a blocking finding whose dispute the operator
//! accepted, or a blocking finding with no review layer to resolve it,
//! filed `high`): tagged `visual-review`, `spawned_from` the delivered
//! task, with the mock, capture and side-by-side attached and its in-line
//! verification recorded as its `finding_verified` entry. It also names
//! the landed tasks a visual review experiment enrolled, whose landed head
//! the post-landing comparison (`pass: measure`) then reviews in both arms.

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_engine::DispatchError;
use orbit_types::task::{
    DefectSeverity, Task, TaskArtifact, TaskComplexity, TaskPriority, TaskRelation,
    TaskRelationType, TaskStatus, TaskType, VISUAL_REVIEW_TAG, VisualCaptureRecord,
    VisualDisposition, VisualPass, VisualReviewFinding, VisualReviewMode, VisualReviewRecord,
    media_type_for_artifact_path,
};
use serde_json::{Value, json};

use super::{attach_system_artifacts, json_artifact, read_record, step_task_ids};
use crate::OrbitRuntime;
use crate::application::task::{
    FINDING_VERIFICATION_SCHEMA_VERSION, FindingEvidence, FindingVerdict, FindingVerification,
    TaskAddParams, TaskUpdateParams, VerdictConfidence, VerdictSource, VerdictWrite,
    VerifierVerdict, finding_fingerprint,
};

pub(crate) fn visual_landed_step(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
) -> Result<Value, DispatchError> {
    let failed = |error: OrbitError| DispatchError::action_failed(action, error.to_string());
    let base_branch = input
        .get("base")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let run_id = input
        .get("job_run_id")
        .and_then(Value::as_str)
        .map(str::to_string);
    let mut filed = Vec::new();
    let mut measure = Vec::new();
    let mut task_ids = step_task_ids(input);
    // The owner landing pipeline lands one claimed task.
    if task_ids.is_empty()
        && let Some(task_id) = input
            .get("task_id")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
    {
        task_ids.push(task_id.trim().to_string());
    }
    for task_id in task_ids {
        let task = runtime.get_task(&task_id).map_err(failed)?;
        if task.status != TaskStatus::Done {
            continue;
        }
        for follow_up in
            file_follow_ups(runtime, &task, &base_branch, run_id.as_deref()).map_err(failed)?
        {
            filed.push(json!({ "task_id": task.id, "follow_up": follow_up }));
        }
        if runtime
            .experiment_visual_reviewer_arm(&task.id)
            .map_err(failed)?
            .is_some()
        {
            measure.push(task.id.to_string());
        }
    }
    Ok(json!({
        "applies": !filed.is_empty() || !measure.is_empty(),
        "filed": filed,
        "measure": !measure.is_empty(),
        "measure_task_ids": measure,
    }))
}

/// File `task`'s unresolved confirmed visual findings, recording each
/// follow-up on its finding so a retried step files nothing twice.
fn file_follow_ups(
    runtime: &OrbitRuntime,
    task: &Task,
    base_branch: &str,
    run_id: Option<&str>,
) -> Result<Vec<String>, OrbitError> {
    let review_path = VisualPass::Delivery.review_artifact();
    let Some((mut record, _)) = read_record::<VisualReviewRecord>(runtime, &task.id, &review_path)?
    else {
        return Ok(Vec::new());
    };
    if record.mode != VisualReviewMode::Live || record.recorded_only {
        return Ok(Vec::new());
    }
    let capture = read_record::<VisualCaptureRecord>(
        runtime,
        &task.id,
        &VisualPass::Delivery.capture_artifact(),
    )?
    .map(|(capture, _)| capture);
    let mut filed = Vec::new();
    for index in 0..record.findings.len() {
        if !record.findings[index].files_follow_up() {
            continue;
        }
        let follow_up = file_follow_up(
            runtime,
            task,
            &record,
            &record.findings[index],
            capture.as_ref(),
            base_branch,
            run_id,
        )?;
        record.findings[index].follow_up = Some(follow_up.clone());
        attach_system_artifacts(
            runtime,
            &task.id,
            vec![json_artifact(&review_path, &record)?],
        )?;
        filed.push(follow_up);
    }
    Ok(filed)
}

fn follow_up_severity(finding: &VisualReviewFinding) -> &str {
    if finding.disposition == Some(VisualDisposition::FollowUpHigh) {
        "high"
    } else {
        finding.severity_of_record()
    }
}

fn file_follow_up(
    runtime: &OrbitRuntime,
    task: &Task,
    record: &VisualReviewRecord,
    finding: &VisualReviewFinding,
    capture: Option<&VisualCaptureRecord>,
    base_branch: &str,
    run_id: Option<&str>,
) -> Result<String, OrbitError> {
    let severity = follow_up_severity(finding);
    let title = format!("Visual review: {} ({})", finding.title.trim(), finding.mock);
    // The owner's transactional action key survives a lost creation reply and
    // does not depend on a capped task listing, mutable title or task status.
    let action_key = format!(
        "visual-follow-up:{}:{}:{}",
        task.id, record.candidate_sha, finding.finding_id
    );
    let existing = runtime
        .stores()
        .tasks()
        .automation_task_for_key(&action_key)?;
    let why = match finding.disposition {
        Some(VisualDisposition::FollowUpHigh) => {
            "It is at or above `visual_review.block_at`, but the delivery ran no review before \
             landing to resolve it, so it is filed `high`."
        }
        Some(VisualDisposition::MustAddress) => {
            "It blocked the delivery; the code reviewer disputed it and the operator accepted \
             the dispute."
        }
        _ => "It was an optional fix the code reviewer did not make.",
    };
    let description = format!(
        "The visual review of {} confirmed this {severity} finding on mock `{}` (question \
         {}) at candidate {}, and the delivery landed without fixing it. {why}\n\n{}\n\n\
         The mock, the capture and the side-by-side are attached.",
        task.id,
        finding.mock,
        finding.question,
        record.candidate_sha,
        finding.detail.trim()
    );
    let acceptance_criteria = vec![format!(
        "The view `{}` renders matches its mock: {}",
        finding.mock,
        finding.title.trim()
    )];
    let filed = if let Some(existing) = existing {
        if existing.title != title
            || existing.description != description
            || existing.acceptance_criteria != acceptance_criteria
            || existing.task_type != TaskType::Bug
            || !existing.tags.iter().any(|tag| tag == VISUAL_REVIEW_TAG)
            || !existing.relations.iter().any(|relation| {
                relation.relation_type == TaskRelationType::SpawnedFrom
                    && relation.target == task.id
            })
        {
            return Err(OrbitError::Execution(format!(
                "visual follow-up '{}' material changed; preserve the operator's changes for review",
                existing.id
            )));
        }
        existing
    } else {
        runtime.add_task_admitted(
            TaskAddParams {
                title,
                description,
                acceptance_criteria,
                relations: vec![TaskRelation {
                    relation_type: TaskRelationType::SpawnedFrom,
                    target: task.id.clone(),
                    regression: None,
                }],
                tags: vec![VISUAL_REVIEW_TAG.to_string()],
                priority: match severity {
                    "critical" => TaskPriority::Critical,
                    "high" => TaskPriority::High,
                    "medium" => TaskPriority::Medium,
                    _ => TaskPriority::Low,
                },
                complexity: TaskComplexity::Unassessed,
                task_type: Some(TaskType::Bug),
                status: Some(TaskStatus::Proposed),
                system_created: true,
                ..TaskAddParams::default()
            },
            None,
            None,
            Some(&action_key),
        )?
    };
    let mut images = Vec::new();
    let mock = runtime
        .get_task_artifact(&task.id, &finding.mock)?
        .ok_or_else(|| OrbitError::Execution("visual follow-up mock evidence is missing".into()))?;
    images.push(mock);
    let captured = capture
        .and_then(|capture| {
            capture
                .mocks
                .iter()
                .find(|mock| mock.artifact == finding.mock)
        })
        .and_then(|mock| mock.capture.as_ref());
    let captured = captured.ok_or_else(|| {
        OrbitError::Execution("visual follow-up capture evidence is missing".into())
    })?;
    for path in [captured.path.as_str(), captured.side_by_side.as_str()] {
        images.push(runtime.get_task_artifact(&task.id, path)?.ok_or_else(|| {
            OrbitError::Execution(format!("visual follow-up evidence '{path}' is missing"))
        })?);
    }
    let images: Vec<TaskArtifact> = images
        .into_iter()
        .map(|artifact| TaskArtifact {
            media_type: media_type_for_artifact_path(&artifact.path).to_string(),
            created_by: None,
            ..artifact
        })
        .collect();
    if !images.is_empty() {
        runtime.update_task_as_system(
            &filed.id,
            TaskUpdateParams {
                upsert_artifacts: images,
                ..Default::default()
            },
            None,
        )?;
    }
    let filed = runtime.get_task(&filed.id)?;
    let confidence = match finding.verdict.confidence.as_deref() {
        Some("high") => VerdictConfidence::High,
        Some("low") => VerdictConfidence::Low,
        _ => VerdictConfidence::Medium,
    };
    let verification = FindingVerification {
        schema_version: FINDING_VERIFICATION_SCHEMA_VERSION,
        source: VerdictSource::VisualReview,
        verdict: FindingVerdict::Confirmed,
        confidence,
        verified_sha: record.candidate_sha.clone(),
        finding_fingerprint: Some(finding_fingerprint(&filed)),
        base_branch: base_branch.to_string(),
        severity: DefectSeverity::parse(severity),
        evidence: finding
            .evidence
            .iter()
            .map(|evidence| {
                let [x, y, w, h] = evidence.region;
                FindingEvidence {
                    location: format!("{}#{x},{y},{w},{h}", evidence.image),
                    note: evidence
                        .location
                        .clone()
                        .unwrap_or_else(|| finding.title.clone()),
                }
            })
            .collect(),
        reasoning: finding.verdict.reasoning.clone(),
        reproduction: None,
        rejection: None,
        crews: finding
            .verdict
            .crew
            .iter()
            .map(|crew| VerifierVerdict {
                crew: crew.clone(),
                tier: TaskComplexity::Unassessed,
                verdict: FindingVerdict::Confirmed,
                confidence,
            })
            .collect(),
        escalated: finding.verdict.escalated,
        run_id: run_id.map(str::to_string),
        verified_at: Utc::now(),
    };
    let history = runtime.get_task_history(&filed.id)?;
    let already_verified = history
        .iter()
        .filter(|entry| entry.event == "finding_verified")
        .filter_map(|entry| entry.note.as_deref())
        .filter_map(|note| serde_json::from_str::<FindingVerification>(note).ok())
        .any(|previous| {
            previous.source == VerdictSource::VisualReview
                && previous.verified_sha == verification.verified_sha
                && previous.finding_fingerprint == verification.finding_fingerprint
        });
    if !already_verified {
        match runtime.record_finding_verification(
            &filed.id,
            &verification,
            filed.status,
            history.len(),
        )? {
            VerdictWrite::Recorded(_) => {}
            VerdictWrite::Superseded { detail, .. } => {
                let matching = runtime
                    .get_task_history(&filed.id)?
                    .iter()
                    .filter(|entry| entry.event == "finding_verified")
                    .filter_map(|entry| entry.note.as_deref())
                    .filter_map(|note| serde_json::from_str::<FindingVerification>(note).ok())
                    .any(|previous| {
                        previous.source == VerdictSource::VisualReview
                            && previous.verified_sha == verification.verified_sha
                            && previous.finding_fingerprint == verification.finding_fingerprint
                    });
                if !matching {
                    return Err(OrbitError::Execution(format!(
                        "visual follow-up verification was superseded: {detail}"
                    )));
                }
            }
        }
    }
    Ok(filed.id.to_string())
}
