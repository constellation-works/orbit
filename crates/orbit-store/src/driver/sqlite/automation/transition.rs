//! Checkpoint transition validation.

use orbit_common::OrbitError;
use orbit_types::workflow::automation::{AcceptedCoverage, AutomationState, Delivery};

use super::members;

pub(super) fn validate_transition(
    previous: &AutomationState,
    next: &AutomationState,
    receipt: Option<&AcceptedCoverage>,
) -> Result<(), OrbitError> {
    let invalid = || OrbitError::InvalidInput("invalid automation checkpoint transition".into());

    if previous.consumer != next.consumer
        || previous.epoch != next.epoch
        || previous.trigger != next.trigger
        || previous.repository != next.repository
        || previous.branch != next.branch
        || previous.baseline != next.baseline
        // The stall marker moves only through its own fenced write.
        || previous.stall != next.stall
        || previous.generation.checked_add(1) != Some(next.generation)
        || next.pending.len() > 1000
        || next.pending_commits.len() > 5000
    {
        return Err(invalid());
    }

    if previous.members.is_some() || next.members.is_some() {
        return members::validate(previous, next, receipt);
    }

    // An attempt may gain retries but never swap the batch it was frozen against,
    // and it may only disappear when a receipt retires it.
    if let Some(old) = &previous.active {
        if let Some(new) = &next.active {
            if old.batch != new.batch
                || old.input_digest != new.input_digest
                || old.reissue != new.reissue
                || new.attempt < old.attempt
            {
                return Err(invalid());
            }
        } else if receipt.is_none() {
            return Err(invalid());
        }
    }

    if let Some(receipt) = receipt {
        let active = previous.active.as_ref().ok_or_else(invalid)?;
        if active.batch.id != receipt.batch_id
            || active.action_id.as_deref() != Some(&receipt.action_id)
            || active.input_digest != receipt.input_digest
            || next.active.is_some()
            || next.covered != active.batch.through_inclusive
            || previous.covered != active.batch.from_exclusive
            || receipt.evidence.is_empty()
            || receipt.submitted_by.is_empty()
        {
            return Err(invalid());
        }

        use orbit_common::security::release::sha256_hex;

        if receipt.evidence_digest != sha256_hex(&receipt.evidence) {
            return Err(invalid());
        }

        // Accepting the batch retires exactly the deliveries it fully covered;
        // anything it only partly covered has to survive the checkpoint.
        let uncovered = |deliveries: &[Delivery]| {
            deliveries
                .iter()
                .filter(|delivery| {
                    !delivery
                        .commits
                        .iter()
                        .all(|sha| active.batch.commits.contains(sha))
                })
                .cloned()
                .collect::<Vec<_>>()
        };

        if next.waived != uncovered(&previous.waived) {
            return Err(invalid());
        }

        if next.pending != uncovered(&previous.pending) {
            return Err(invalid());
        }

        let retained_exclusions = previous
            .excluded
            .iter()
            .filter(|excluded| {
                !excluded
                    .delivery
                    .commits
                    .iter()
                    .all(|sha| active.batch.commits.contains(sha))
            })
            .cloned()
            .collect::<Vec<_>>();
        if next.excluded != retained_exclusions {
            return Err(invalid());
        }

        if !previous.pending_commits.starts_with(&active.batch.commits)
            || next.pending_commits != previous.pending_commits[active.batch.commits.len()..]
        {
            return Err(invalid());
        }
    } else if previous.covered != next.covered {
        // A proven-covered prefix may advance the scheduling cursor without a
        // consumer examination receipt. Observation remains append-only.
        validate_excluded_prefix_retirement(previous, next)?;
    } else {
        // Without a receipt the covered cursor stands still: observation may only
        // append commits and retain every delivery already pending.
        if previous.waived != next.waived
            || !next.pending_commits.starts_with(&previous.pending_commits)
            || previous
                .pending
                .iter()
                .any(|delivery| !next.pending.contains(delivery))
            || previous
                .excluded
                .iter()
                .any(|excluded| !next.excluded.contains(excluded))
        {
            return Err(invalid());
        }

        // A newly claimed batch must be an exact prefix of what is already pending.
        if previous.active.is_none()
            && let Some(active) = &next.active
        {
            let batch = &active.batch;
            if batch.consumer != next.consumer
                || batch.epoch != next.epoch
                || batch.repository != next.repository
                || batch.branch != next.branch
                || batch.from_exclusive != next.covered
                || batch.commits.is_empty()
                || !next.pending_commits.starts_with(&batch.commits)
                || batch.commits.last() != Some(&batch.through_inclusive.commit)
                || batch
                    .deliveries
                    .iter()
                    .any(|delivery| !next.pending.contains(delivery))
            {
                return Err(invalid());
            }
        }
    }

    Ok(())
}

fn validate_excluded_prefix_retirement(
    previous: &AutomationState,
    next: &AutomationState,
) -> Result<(), OrbitError> {
    let invalid = || OrbitError::InvalidInput("invalid automation checkpoint transition".into());

    if previous.active.is_some()
        || next.active.is_some()
        || previous.observed != next.observed
        || previous.pending != next.pending
        || previous.waived != next.waived
        || previous.pending_commits.len() <= next.pending_commits.len()
        || !previous.pending_commits.ends_with(&next.pending_commits)
    {
        return Err(invalid());
    }

    let end = previous.pending_commits.len() - next.pending_commits.len();
    let prefix = &previous.pending_commits[..end];
    if prefix.last() != Some(&next.covered.commit) {
        return Err(invalid());
    }

    let pending: std::collections::HashSet<&str> = previous
        .pending
        .iter()
        .flat_map(|delivery| delivery.commits.iter().map(String::as_str))
        .collect();
    if prefix
        .iter()
        .any(|sha| previous.unresolved.contains_key(sha) || pending.contains(sha.as_str()))
    {
        return Err(invalid());
    }

    let closed = previous.excluded.iter().any(|excluded| {
        excluded.delivery.after == next.covered
            && excluded
                .delivery
                .commits
                .iter()
                .all(|sha| prefix.contains(sha))
    });
    if !closed {
        return Err(invalid());
    }

    if previous.excluded.iter().any(|excluded| {
        let hits = excluded
            .delivery
            .commits
            .iter()
            .any(|sha| prefix.contains(sha));
        let whole = excluded
            .delivery
            .commits
            .iter()
            .all(|sha| prefix.contains(sha));
        hits && !whole
    }) || prefix.iter().any(|sha| {
        !previous
            .excluded
            .iter()
            .any(|excluded| excluded.delivery.commits.iter().any(|commit| commit == sha))
    }) {
        return Err(invalid());
    }

    let retained = previous
        .excluded
        .iter()
        .filter(|excluded| {
            !excluded
                .delivery
                .commits
                .iter()
                .all(|sha| prefix.contains(sha))
        })
        .cloned()
        .collect::<Vec<_>>();
    if next.excluded != retained {
        return Err(invalid());
    }

    let mut unresolved = previous.unresolved.clone();
    let mut associations = previous.associations.clone();
    for sha in prefix {
        unresolved.remove(sha);
        associations.remove(sha);
    }
    if next.unresolved != unresolved || next.associations != associations {
        return Err(invalid());
    }

    Ok(())
}
