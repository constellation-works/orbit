//! Validate and record rejection of review findings judged invalid by the pilot.
use super::super::FINDING_INVALID;
use super::super::persist::task_outcome;
use super::ApplyContext;
use super::admission::ValidatedTask;
use super::task::Settled;
use crate::application::task::{
    AnalyzedFinding, InvalidFinding, REVIEW_FINDING_TAGS, VerdictConfidence, VerdictSource,
    VerdictWrite, is_review_finding,
};
use orbit_types::task::Task;
use serde_json::{Value, json};

/// A `finding_invalid` result judges a review-filed bug at the pinned
/// revision, so on any other task, or without a pinned revision, the whole
/// assessment is invalid and nothing is written. Its evidence was already
/// required to be non-empty by `validate_recommendations`.
pub(super) fn invalid_finding_target(
    context: &ApplyContext<'_>,
    task_id: &str,
    assessment: &Value,
    current: &Task,
) -> Settled<()> {
    if assessment.get(FINDING_INVALID).is_none_or(Value::is_null) {
        return Ok(());
    }
    let refusal = if !is_review_finding(current) {
        format!(
            "task {task_id} {FINDING_INVALID} applies only to a review-filed bug (type bug \
             tagged {}); return null for any other task",
            REVIEW_FINDING_TAGS.join(", ")
        )
    } else if context.source.is_none() {
        format!(
            "task {task_id} {FINDING_INVALID} needs a pinned source revision to cite; a non-Git \
             workspace returns null"
        )
    } else {
        return Ok(());
    };
    Err(task_outcome(task_id, "invalid", Some(refusal)))
}

/// Record the pilot's `finding_invalid` as a REJECTED finding verdict once
/// its assessment is written, which moves the task to `rejected`. The
/// outcome is reported on the assessment as `finding_rejection`; the pilot
/// write already stands, so a rejection that does not record is reported
/// rather than failing the apply.
pub(super) fn reject_invalid_finding(context: &ApplyContext<'_>, validated: &mut ValidatedTask) {
    let Some(evidence) = validated
        .assessment
        .get(FINDING_INVALID)
        .and_then(|finding| finding.get("evidence"))
        .and_then(Value::as_str)
        .map(str::to_string)
    else {
        return;
    };
    let Some(source) = context.source else {
        return;
    };
    let confidence = match validated.assessment["confidence"].as_str() {
        Some("high") => VerdictConfidence::High,
        Some("low") => VerdictConfidence::Low,
        _ => VerdictConfidence::Medium,
    };
    #[cfg(test)]
    super::super::persist::lock_hook::fire(
        super::super::persist::lock_hook::Point::BeforeFindingRejection,
    );
    // The receipt holds what the locked pilot write left; history past it
    // happened after the judgement, on first apply and on replay alike.
    let sealed = validated
        .applied_material
        .as_ref()
        .and_then(|material| Some((material.finding_fingerprint.clone()?, material.status?)));
    let boundary = context
        .runtime
        .get_task_history(&validated.task_id)
        .map(|history| {
            history
                .iter()
                .position(|event| {
                    event.event == "task_pilot_applied"
                        && event.receipt() == Some(validated.operation_id.as_str())
                })
                .map(|position| position + 1)
        });
    let (fingerprint, status, boundary) = match (sealed, boundary) {
        (Some((fingerprint, status)), Ok(Some(boundary))) => (fingerprint, status, boundary),
        (_, Err(error)) => {
            record_finding_rejection(
                validated,
                json!({ "recorded": false, "reason": "apply_failed", "detail": error.to_string() }),
            );
            return;
        }
        _ => {
            record_finding_rejection(
                validated,
                json!({
                    "recorded": false,
                    "reason": "analyzed_snapshot_missing",
                    "detail": "the pilot's receipt seals no finding material to bind the rejection to",
                }),
            );
            return;
        }
    };
    let finding = InvalidFinding {
        source: VerdictSource::TaskPilot,
        analyzed: AnalyzedFinding {
            fingerprint: &fingerprint,
            status,
            history_boundary: Some(boundary),
        },
        verified_sha: &source.source_revision,
        base_branch: &source.base_branch,
        confidence,
        detail: &evidence,
        evidence: Vec::new(),
        crew: validated.pilot_crew.as_deref(),
        run_id: validated.pilot_run_id.as_deref(),
    };
    let report = match context
        .runtime
        .reject_invalid_finding(&validated.task_id, &finding)
    {
        Ok(VerdictWrite::Recorded(status)) => json!({ "recorded": true, "status": status }),
        Ok(VerdictWrite::Superseded { reason, detail }) => {
            json!({ "recorded": false, "reason": reason, "detail": detail })
        }
        Err(error) => {
            tracing::warn!(
                target: "orbit.core.task_pilot",
                task_id = %validated.task_id,
                %error,
                "could not reject a review finding task-pilot judged invalid"
            );
            json!({ "recorded": false, "reason": "apply_failed", "detail": error.to_string() })
        }
    };
    record_finding_rejection(validated, report);
}

fn record_finding_rejection(validated: &mut ValidatedTask, report: Value) {
    if let Value::Object(fields) = &mut validated.assessment {
        fields.insert("finding_rejection".to_string(), report);
    }
}
