//! Whether a decision request can take an answer, and the answer comment.

use orbit_common::{DecisionRefusalCode, OrbitError};
use orbit_types::task::{DecisionRequest, DecisionRequestState};

/// Refuse an answer to `request` as it reads now: one that is not open
/// (expired, answered or superseded) or whose answer it does not allow.
/// Nothing is written.
pub(in crate::application::task) fn ensure_request_answerable(
    request: &DecisionRequest,
    answer_id: &str,
) -> Result<(), OrbitError> {
    let request_id = request.id.as_str();
    match request.state {
        DecisionRequestState::Open => {}
        DecisionRequestState::Expired => {
            return Err(OrbitError::decision_refused(
                DecisionRefusalCode::RequestExpired,
                format!(
                    "decision request `{request_id}` expired at {}; nothing was applied",
                    request.expires_at.to_rfc3339()
                ),
            ));
        }
        DecisionRequestState::Answered => {
            let by = request
                .resolution
                .as_ref()
                .map_or("another answer", |resolution| {
                    resolution.answered_by.as_str()
                });
            return Err(OrbitError::decision_refused(
                DecisionRefusalCode::RequestSuperseded,
                format!(
                    "decision request `{request_id}` was already answered by {by}; nothing was applied"
                ),
            ));
        }
        DecisionRequestState::Superseded => {
            return Err(OrbitError::decision_refused(
                DecisionRefusalCode::RequestSuperseded,
                format!(
                    "decision request `{request_id}` no longer matches the hold on task {}; nothing was applied",
                    request.task_id
                ),
            ));
        }
    }
    if !request.allows(answer_id) {
        let allowed = request
            .answers
            .iter()
            .map(|answer| answer.id.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(OrbitError::decision_refused(
            DecisionRefusalCode::AnswerNotAllowed,
            format!(
                "`{answer_id}` is not an allowed answer for decision request `{request_id}` (allowed: {allowed}); nothing was applied"
            ),
        ));
    }
    Ok(())
}

/// The human comment an applied answer records beside its own effect.
pub(in crate::application::task) fn decision_comment(
    request: &DecisionRequest,
    answer_id: &str,
    label: &str,
    via: &str,
    note: Option<&str>,
    consequence: &str,
) -> String {
    let mut comment = format!(
        "Decision answer: {label} (`{answer_id}`) for decision request {} via {via}.",
        request.id
    );
    if !consequence.is_empty() {
        comment.push('\n');
        comment.push_str(consequence);
    }
    if let Some(note) = note {
        comment.push('\n');
        comment.push_str(note);
    }
    comment
}
