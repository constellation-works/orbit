use clap::Args;
use orbit_core::Core;
use orbit_core::app::doctor::{Command, Outcome, exec};
use serde_json::json;

use crate::command::{Block, CommandOut, Payload};

#[derive(Args)]
pub struct AdmissionReceiptsArgs {
    /// Compact idle and settled admission receipts to permanent tombstones; retain full receipts
    /// for unsettled claims and failed or revoked claims with a preserved PR.
    #[arg(long)]
    pub compact: bool,
}

pub(super) fn admission_receipts(core: &Core, compact: bool) -> CommandOut {
    let Outcome::AdmissionReceipts {
        compacted_receipts,
        storage,
    } = exec(core, Command::AdmissionReceipts { compact })?
    else {
        unreachable!("an admission-receipts command answers with its storage")
    };
    let text = format!(
        "Compacted admission receipts: {compacted_receipts}\nFull admission receipts: {} ({} logical payload bytes)\nPermanent tombstones: {} ({} logical payload bytes)\nClaim mutation receipts: {} ({} logical payload bytes; pruned separately on settlement after 7 days)\nLogical payload bytes exclude row keys, indexes, free pages and WAL.",
        storage.full_receipts.count,
        storage.full_receipts.logical_payload_bytes,
        storage.tombstones.count,
        storage.tombstones.logical_payload_bytes,
        storage.claim_mutation_receipts.count,
        storage.claim_mutation_receipts.logical_payload_bytes,
    );
    Ok(Payload::blocks(
        json!({"compacted_receipts": compacted_receipts, "storage": storage}),
        vec![Block::prose(text)],
    )
    .into())
}
