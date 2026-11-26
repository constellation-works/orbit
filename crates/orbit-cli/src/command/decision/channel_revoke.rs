use orbit_core::app::task::Outcome;

use super::support::serialize;
use crate::command::{CommandOut, Payload};
use crate::output::common::payload::Block;

pub(super) fn render(outcome: Outcome) -> CommandOut {
    let info = outcome.into_channel()?;
    let blocks = vec![Block::text(format!(
        "Revoked decision channel `{}`; its outstanding tokens no longer verify.",
        info.channel
    ))];
    Ok(Payload::blocks(serialize(&info)?, blocks).into())
}
