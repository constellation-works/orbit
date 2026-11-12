use orbit_core::OrbitRuntime;
use serde_json::json;

use crate::command::{Block, CommandOut, Payload};

pub(super) fn admission_receipts(runtime: &OrbitRuntime, compact: bool) -> CommandOut {
    let (compacted_receipts, storage) = if compact {
        let report = runtime.compact_admission_receipts()?;
        (report.compacted_receipts, report.storage)
    } else {
        (0, runtime.inspect_admission_receipt_storage()?)
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
