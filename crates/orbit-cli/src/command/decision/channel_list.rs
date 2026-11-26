use orbit_core::app::task::Outcome;

use super::support::serialize;
use crate::command::{CommandOut, Payload};
use crate::output::common::payload::Block;
use crate::output::common::table::{Column, Table};

pub(super) fn render(outcome: Outcome) -> CommandOut {
    let channels = outcome.into_channels()?;
    let value = serialize(&channels)?;
    let mut table = Table::new(vec![
        Column::new("CHANNEL").fixed(),
        Column::new("HUMAN").fixed(),
        Column::new("PLUGIN").fixed(),
        Column::new("EPOCH").fixed(),
        Column::new("STATE").fixed(),
        Column::new("DKIM").fixed(),
        Column::new("SENDERS"),
    ])
    .empty_message("no decision channels registered on this host");
    for channel in &channels {
        table.add_row(vec![
            channel.channel.clone(),
            channel.human.clone(),
            channel.plugin.clone(),
            channel.epoch.to_string(),
            if channel.revoked { "revoked" } else { "active" }.to_string(),
            if channel.require_dkim_verification {
                "required"
            } else {
                "optional"
            }
            .to_string(),
            channel.allowed_senders.join(", "),
        ]);
    }
    Ok(Payload::blocks(value, vec![Block::table(table)]).into())
}
