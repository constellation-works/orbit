//! The crew memory write rules, run inside one `Store::write` transaction so
//! the number a revision takes, the per-run proposal cap and a decision's
//! freshness check cannot interleave with another writer.

use orbit_common::OrbitError;
use orbit_store::Tx;
use orbit_store::contracts::{
    CrewMemoryEventRow, CrewMemoryProposalDecision, CrewMemoryProposalOutcome,
    CrewMemoryProposalRow, CrewMemoryRevisionRow,
};

/// Append `revision` as its key's next revision and `event` with that number.
/// The `revision` fields of both inputs are ignored. Returns the stored
/// revision.
pub(super) fn append_revision(
    tx: &Tx<'_>,
    revision: &CrewMemoryRevisionRow,
    event: &CrewMemoryEventRow,
) -> Result<CrewMemoryRevisionRow, OrbitError> {
    let stored = put_next_revision(tx, revision)?;
    let mut event = event.clone();
    event.revision = Some(stored.revision);
    tx.crew_memory().put_event(&event)?;
    Ok(stored)
}

/// Insert `proposal` and `event`, unless the proposing run already holds
/// `max_per_run` proposals in the workspace.
pub fn open_proposal(
    tx: &Tx<'_>,
    proposal: &CrewMemoryProposalRow,
    event: &CrewMemoryEventRow,
    max_per_run: u32,
) -> Result<CrewMemoryProposalOutcome, OrbitError> {
    let memory = tx.crew_memory();
    if let Some(run_id) = &proposal.proposing_run_id {
        let existing = memory.count_run_proposals(&proposal.workspace_id, run_id)?;
        if existing >= max_per_run {
            return Ok(CrewMemoryProposalOutcome::CapReached { existing });
        }
    }
    memory.put_proposal(proposal)?;
    memory.put_event(event)?;
    Ok(CrewMemoryProposalOutcome::Opened)
}

/// Decide an open proposal and append its audit event. An acceptance also
/// appends its global revision. The tuple's number is the expected current
/// global revision (0 if empty); a stale read is refused rather than
/// overwriting intervening operator work.
pub fn decide_proposal(
    tx: &Tx<'_>,
    decision: &CrewMemoryProposalDecision,
    event: &CrewMemoryEventRow,
    revision: Option<(&CrewMemoryRevisionRow, u32)>,
) -> Result<Option<CrewMemoryRevisionRow>, OrbitError> {
    let memory = tx.crew_memory();
    if !memory.decide_proposal(decision)? {
        return Err(OrbitError::InvalidInput(format!(
            "crew memory proposal `{}` is not open or does not exist",
            decision.id
        )));
    }
    let stored = if let Some((row, expected)) = revision {
        let current = memory.latest_revision_number(&row.workspace_id, &row.memory_key)?;
        if current != expected {
            return Err(OrbitError::InvalidInput(format!(
                "global memory changed from revision {expected} to {current}; read it again before accepting the proposal"
            )));
        }
        Some(put_next_revision(tx, row)?)
    } else {
        None
    };
    let mut event = event.clone();
    event.revision = stored.as_ref().map(|row| row.revision);
    memory.put_event(&event)?;
    Ok(stored)
}

/// Store `revision` numbered one past its key's newest revision.
fn put_next_revision(
    tx: &Tx<'_>,
    revision: &CrewMemoryRevisionRow,
) -> Result<CrewMemoryRevisionRow, OrbitError> {
    let memory = tx.crew_memory();
    let mut stored = revision.clone();
    stored.revision = memory
        .latest_revision_number(&revision.workspace_id, &revision.memory_key)?
        .saturating_add(1);
    memory.put_revision(&stored)?;
    Ok(stored)
}
