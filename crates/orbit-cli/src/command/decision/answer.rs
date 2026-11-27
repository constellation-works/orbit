use orbit_common::OrbitError;
use orbit_core::app::task::Outcome;

use crate::command::{CommandOut, Payload};
use crate::output::common::payload::Block;

pub(super) fn render(outcome: Outcome, answer_text: Option<String>) -> CommandOut {
    let request = outcome.into_decision_request()?;
    let value = serde_json::to_value(&request)
        .map_err(|error| OrbitError::Execution(format!("serialize decision request: {error}")))?;
    let blocks = vec![Block::text(format!(
        "Answered {} with `{}`; the request on {} now reads {}.",
        request.id,
        answer_text.unwrap_or_default(),
        request.task_id,
        request.state.as_str(),
    ))];
    Ok(Payload::blocks(value, blocks).into())
}
