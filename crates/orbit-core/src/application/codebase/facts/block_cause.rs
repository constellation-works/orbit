//! Failed claim settlements read for the block classification.
//!
//! The classification itself is `task`'s ([`crate::application::task`]'s
//! `block_cause` leaf); this module reads what it needs from `distributed`,
//! which sits above `task`, and re-exports the classifier for `codebase` and
//! `agents` consumers.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use orbit_store::contracts::{ClaimSettlementKind, ExecutionClaimPhase};

use crate::OrbitRuntime;
use crate::application::distributed::LeafSettlement;
pub(in crate::application) use crate::application::task::{
    SettledFailure, actor_kind, classify_block, entry_marks_run,
};

/// Every failed claim's settlement, by task, for `claim_failed` entries
/// written before their note named the cause. An owner whose claims cannot
/// be read classifies those entries as unclassified rather than failing.
pub(in crate::application) fn failed_claim_settlements(
    runtime: &OrbitRuntime,
) -> BTreeMap<String, Vec<SettledFailure>> {
    let settlements = runtime
        .leaf_settlements(None, false)
        .unwrap_or_else(|error| {
            tracing::warn!(%error, "codebase analytics: claim settlements unreadable");
            Vec::new()
        });
    failed_claim_settlements_of(&settlements)
}

/// [`failed_claim_settlements`] of settlements already read. A typed
/// handoff is never a failure, whatever its claim's phase now.
pub(in crate::application) fn failed_claim_settlements_of(
    settlements: &[LeafSettlement],
) -> BTreeMap<String, Vec<SettledFailure>> {
    let mut by_task: BTreeMap<String, Vec<SettledFailure>> = BTreeMap::new();
    for settlement in settlements {
        if settlement.phase != ExecutionClaimPhase::Failed
            || settlement.kind == Some(ClaimSettlementKind::Handoff)
        {
            continue;
        }
        let Ok(settled_at) = DateTime::parse_from_rfc3339(&settlement.settled_at) else {
            continue;
        };
        by_task.entry(settlement.task_id.clone()).or_default().push(
            SettledFailure::from_settlement_record(
                settled_at.with_timezone(&Utc),
                settlement.evidence.to_string(),
                settlement.failure_class,
                settlement.leaf_run_id.is_some(),
                settlement.reason.clone().unwrap_or_default(),
            ),
        );
    }
    by_task
}
