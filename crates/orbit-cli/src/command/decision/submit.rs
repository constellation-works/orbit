use orbit_core::app::task::Outcome;

use super::support::text;
use crate::command::{CommandOut, Payload};
use crate::output::common::payload::Block;

pub(super) fn render(outcome: Outcome) -> CommandOut {
    let value = outcome.into_answer()?;
    let blocks = vec![Block::text(format!(
        "Submitted `{}` for {}; the request on {} now reads {}.",
        text(&value["resolution"]["answer_id"]),
        text(&value["id"]),
        text(&value["task_id"]),
        text(&value["state"]),
    ))];
    Ok(Payload::blocks(value, blocks).into())
}
