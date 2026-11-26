use orbit_core::app::task::Outcome;

use super::support::serialize;
use crate::command::{CommandOut, Payload};
use crate::output::common::payload::Block;

pub(super) fn render(outcome: Outcome) -> CommandOut {
    let info = outcome.into_channel()?;
    let blocks = vec![Block::text(format!(
        "Registered decision channel `{}` for {} via plugin {} (key epoch {}, fingerprint sha256:{}).",
        info.channel, info.human, info.plugin, info.epoch, info.key_fingerprint
    ))];
    Ok(Payload::blocks(serialize(&info)?, blocks).into())
}
