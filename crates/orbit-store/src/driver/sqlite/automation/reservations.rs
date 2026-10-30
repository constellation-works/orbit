//! Validation for bounded independent review reservations.

use orbit_common::{OrbitError, security::release::sha256_hex};
use orbit_types::workflow::automation::*;

pub(super) fn validate_reservations(
    previous: &AutomationState,
    next: &AutomationState,
    receipt: Option<&AcceptedCoverage>,
) -> Result<(), OrbitError> {
    let invalid =
        || OrbitError::InvalidInput("invalid independent review checkpoint transition".into());
    if !next.independent_review
        || next.members.is_some()
        || next.delivery_attempts().count() > MAX_DELIVERY_RESERVATIONS
        || (next.active.is_none() && !next.reservations.is_empty())
    {
        return Err(invalid());
    }
    if !previous.independent_review {
        let mut expected = previous.clone();
        expected.generation = next.generation;
        expected.independent_review = true;
        return if &expected == next
            && receipt.is_none()
            && next
                .trigger
                .as_ref()
                .map(|t| t.coverage)
                .or_else(|| next.active.as_ref().map(|a| a.batch.coverage))
                == Some(CoverageClass::LandedCodeReviewV1)
        {
            Ok(())
        } else {
            Err(invalid())
        };
    }
    // Acceptance records one batch's genuine receipt and keeps the observation
    // window and every other reservation untouched, including earlier gaps.
    if let Some(receipt) = receipt {
        let old = previous
            .delivery_attempts()
            .find(|a| a.batch.id == receipt.batch_id)
            .ok_or_else(invalid)?;
        if !matches!(old.state, BatchState::Claimed | BatchState::Admitted)
            || old.action_id.as_deref() != Some(receipt.action_id.as_str())
            || old.input_digest != receipt.input_digest
            || receipt.evidence.is_empty()
            || receipt.submitted_by.is_empty()
            || receipt.evidence_digest != sha256_hex(&receipt.evidence)
        {
            return Err(invalid());
        }
        let mut expected = previous.clone();
        expected.generation = next.generation;
        let accepted = expected
            .delivery_attempt_mut(&receipt.batch_id)
            .ok_or_else(invalid)?;
        accepted.state = BatchState::Covered;
        accepted.reason = None;
        if &expected == next {
            return Ok(());
        }
        // Preserve the public Store contract for an oldest receipt accepted
        // and retired in one transaction. A later receipt cannot use this
        // path, and retirement still compares the entire retained window.
        if previous
            .active
            .as_ref()
            .is_some_and(|a| a.batch.id == receipt.batch_id)
            && previous.covered != next.covered
        {
            return validate_reservations(&expected, next, None);
        }
        return Err(invalid());
    }
    // Only the consecutive accepted prefix can leave the retained window.
    if previous.covered != next.covered {
        if previous.active.is_none()
            && next.active.is_none()
            && previous.reservations.is_empty()
            && next.reservations.is_empty()
        {
            return super::transition::validate_excluded_prefix_retirement(previous, next);
        }
        let active = previous
            .active
            .as_ref()
            .filter(|a| a.state == BatchState::Covered)
            .ok_or_else(invalid)?;
        let commits = &active.batch.commits;
        if active.batch.from_exclusive != previous.covered
            || !previous.pending_commits.starts_with(commits)
            || commits.is_empty()
        {
            return Err(invalid());
        }
        let mut expected = previous.clone();
        expected.generation = next.generation;
        expected.covered = active.batch.through_inclusive.clone();
        expected.pending_commits = expected.pending_commits[commits.len()..].to_vec();
        expected
            .pending
            .retain(|d| !d.commits.iter().all(|sha| commits.contains(sha)));
        expected
            .waived
            .retain(|d| !d.commits.iter().all(|sha| commits.contains(sha)));
        expected
            .excluded
            .retain(|d| !d.delivery.commits.iter().all(|sha| commits.contains(sha)));
        expected.unresolved.retain(|sha, _| !commits.contains(sha));
        expected
            .associations
            .retain(|sha, _| !commits.contains(sha));
        expected
            .lookup_retries
            .retain(|sha, _| !commits.contains(sha));
        expected.active = if expected.reservations.is_empty() {
            None
        } else {
            Some(expected.reservations.remove(0))
        };
        return if &expected == next {
            Ok(())
        } else {
            Err(invalid())
        };
    }
    let old = previous.delivery_attempts().collect::<Vec<_>>();
    let new = next.delivery_attempts().collect::<Vec<_>>();
    if new.len() < old.len() || new.len() > old.len() + 1 {
        return Err(invalid());
    }
    if new.len() == old.len() + 1 {
        if old != new[..old.len()] {
            return Err(invalid());
        }
        let attempt = new.last().ok_or_else(invalid)?;
        let batch = &attempt.batch;
        let start = previous.reserved_commit_count();
        let serialized = serde_json::to_vec(batch).map_err(|_| invalid())?;
        let mut unidentified = batch.clone();
        unidentified.id.clear();
        let identity = serde_json::to_vec(&unidentified).map_err(|_| invalid())?;
        if attempt.input_digest != sha256_hex(&serialized)
            || batch.id != sha256_hex(&identity)
            || batch.coverage != CoverageClass::LandedCodeReviewV1
            || batch.deliveries.len() > 50
            || next.trigger.as_ref().is_some_and(|trigger| {
                batch.coverage != trigger.coverage || batch.deliveries.len() > trigger.max_items
            })
        {
            return Err(invalid());
        }
        if batch.consumer != next.consumer
            || batch.epoch != next.epoch
            || batch.repository != next.repository
            || batch.branch != next.branch
            || batch.from_exclusive != *previous.reserved_through()
            || batch.commits.is_empty()
            || batch.deliveries.is_empty()
            || !previous
                .pending_commits
                .get(start..)
                .is_some_and(|suffix| suffix.starts_with(&batch.commits))
            || batch.commits.last() != Some(&batch.through_inclusive.commit)
            || attempt.state != BatchState::Claimed
            || attempt.attempt != 1
            || attempt.action_key != format!("automation:{}:1", batch.id)
            || attempt.action_id.is_some()
            || attempt.reissue.is_some()
            || batch.deliveries.iter().any(|d| {
                !previous.pending.contains(d)
                    || !d.commits.iter().all(|sha| batch.commits.contains(sha))
            })
            || previous.pending.iter().any(|d| {
                d.commits.iter().any(|sha| batch.commits.contains(sha))
                    && !batch.deliveries.contains(d)
            })
        {
            return Err(invalid());
        }
        let mut expected = previous.clone();
        expected.generation = next.generation;
        if expected.active.is_none() {
            expected.active = Some((*attempt).clone());
        } else {
            expected.reservations.push((*attempt).clone());
        }
        return if &expected == next {
            Ok(())
        } else {
            Err(invalid())
        };
    }
    let changed = old
        .iter()
        .zip(&new)
        .filter(|(a, b)| a != b)
        .collect::<Vec<_>>();
    if !changed.is_empty() {
        if changed.len() != 1 {
            return Err(invalid());
        }
        let (old, new) = changed[0];
        if old.batch != new.batch
            || old.input_digest != new.input_digest
            || old.reissue != new.reissue
            || !matches!(old.state, BatchState::Claimed | BatchState::Admitted)
            || !matches!(
                new.state,
                BatchState::Claimed
                    | BatchState::Admitted
                    | BatchState::Failed
                    | BatchState::Exhausted
            )
            || new.attempt < old.attempt
            || new.attempt > old.attempt.saturating_add(1)
            || new.attempt > old.batch.max_attempts.max(old.attempt)
            || new.action_key != format!("automation:{}:{}", new.batch.id, new.attempt)
            || (new.attempt == old.attempt
                && old.action_id.is_some()
                && old.action_id != new.action_id)
            || (new.attempt != old.attempt
                && (new.state != BatchState::Claimed
                    || new.action_id.is_some()
                    || new.retry_after.is_none()))
        {
            return Err(invalid());
        }
        let mut expected = previous.clone();
        expected.generation = next.generation;
        *expected
            .delivery_attempt_mut(&old.batch.id)
            .ok_or_else(invalid)? = (*new).clone();
        return if &expected == next {
            Ok(())
        } else {
            Err(invalid())
        };
    }
    // Observation may append and resolve attribution, never erase retained debt.
    if previous.waived != next.waived
        || !next.pending_commits.starts_with(&previous.pending_commits)
        || previous.pending.iter().any(|d| !next.pending.contains(d))
        || previous.excluded.iter().any(|d| !next.excluded.contains(d))
        || previous.unresolved.keys().any(|sha| {
            !next.unresolved.contains_key(sha)
                && !next.pending.iter().any(|d| d.commits.contains(sha))
                && !next
                    .excluded
                    .iter()
                    .any(|d| d.delivery.commits.contains(sha))
        })
    {
        return Err(invalid());
    }
    Ok(())
}
