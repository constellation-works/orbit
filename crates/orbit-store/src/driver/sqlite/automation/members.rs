//! State-member invariants use the same transaction and receipt tables.

use orbit_common::OrbitError;
use orbit_types::workflow::automation::{
    AcceptedCoverage, AutomationState,
    members::{MemberAttempt, MemberBatchEvidence, MemberState},
};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

pub(super) fn validate(
    previous: &AutomationState,
    next: &AutomationState,
    receipt: Option<&AcceptedCoverage>,
) -> Result<(), OrbitError> {
    let invalid = || OrbitError::InvalidInput("invalid member checkpoint transition".into());
    let old = previous.members.as_ref().ok_or_else(invalid)?;
    let new = next.members.as_ref().ok_or_else(invalid)?;

    // A member checkpoint touches member state only; the delivery projection and
    // its cursors must come through untouched.
    if previous.active.is_some()
        || next.active.is_some()
        || previous.covered != next.covered
        || previous.observed != next.observed
        || previous.pending != next.pending
        || previous.pending_commits != next.pending_commits
        || previous.waived != next.waived
        || new.pending.len() + new.assessed.len() + new.withheld.len() > 1000
        || new.failed.len() > 1000
    {
        return Err(invalid());
    }

    // A failed record is permanent, except the ones the active attempt just
    // exhausted itself into for members it carried.
    for (key, failed) in &old.failed {
        if new.failed.get(key) != Some(failed)
            && !old.active.as_ref().is_some_and(|active| {
                active.member_for(key).is_some()
                    && new.active.is_none()
                    && new
                        .failed
                        .get(key)
                        .is_some_and(|next| next.id == active.id && next.exhausted)
            })
        {
            return Err(invalid());
        }
    }

    // An in-flight attempt keeps its identity and may only advance by one retry;
    // before admission it may shrink to the members still admissible.
    if let Some(active) = &old.active {
        if let Some(updated) = &new.active {
            let batch_kept = active.members() == updated.members();
            let batch_shrunk = active.action_id.is_none()
                && updated.attempt == active.attempt
                && updated.batch_is_consistent()
                && updated.members().len() < active.members().len()
                && updated.members().iter().all(|member| {
                    active
                        .member_for(&member.key)
                        .is_some_and(|carried| carried == member)
                });
            if active.consumer != updated.consumer
                || active.kind != updated.kind
                || active.id != updated.id
                || !(batch_kept || batch_shrunk)
                || active.max_attempts != updated.max_attempts
                || active.deadline != updated.deadline
                || updated.attempt < active.attempt
                || updated.attempt > active.attempt + 1
                || (updated.attempt == active.attempt
                    && (updated.action_key != active.action_key
                        || (active.action_id.is_some() && updated.action_id != active.action_id)))
                || (updated.attempt > active.attempt
                    && (updated.action_id.is_some()
                        || updated.action_key
                            != format!("automation:{}:{}", active.id, updated.attempt)))
                || updated.attempt > active.max_attempts
            {
                return Err(invalid());
            }
        } else if receipt.is_none() && !every_member_retired(active, new, &BTreeSet::new()) {
            return Err(invalid());
        }
    }

    // A newly claimed attempt starts at attempt 1, unadmitted, over pending members.
    if old.active.is_none()
        && let Some(active) = &new.active
        && (active.consumer != previous.consumer
            || active.attempt != 1
            || active.max_attempts == 0
            || active.max_attempts > 6
            || !active.batch_is_consistent()
            || !active
                .members()
                .iter()
                .all(|member| new.pending.values().any(|pending| pending == member))
            || active.action_id.is_some()
            || active.exhausted
            || active.deadline <= active.retry_after)
    {
        return Err(invalid());
    }

    if let Some(receipt) = receipt {
        let active = old.active.as_ref().ok_or_else(invalid)?;
        let evidence: MemberBatchEvidence =
            serde_json::from_slice(&receipt.evidence).map_err(|_| invalid())?;
        let bytes = serde_json::to_vec(active).map_err(|_| invalid())?;

        if receipt.batch_id != active.id
            || active.action_id.as_ref() != Some(&receipt.action_id)
            || receipt.input_digest != format!("{:x}", Sha256::digest(bytes))
            || receipt.evidence_digest != format!("{:x}", Sha256::digest(&receipt.evidence))
            || evidence.action_id != receipt.action_id
            || evidence.attempt_id != active.id
            || evidence.applied.is_empty()
            || new.active.is_some()
            || receipt.submitted_by != "system"
        {
            return Err(invalid());
        }

        // The receipt adds exactly one assessment per member it applied, each
        // certifying that member's own input, and retires every other member.
        let mut expected_assessments = old.assessed.clone();
        let mut applied = BTreeSet::new();
        for applied_member in &evidence.applied {
            let member = active
                .member_for(&applied_member.member_key)
                .ok_or_else(invalid)?;
            let assessment = new
                .assessed
                .get(&applied_member.member_key)
                .ok_or_else(invalid)?;
            if applied_member.action_id != receipt.action_id
                || applied_member.attempt_id != active.id
                || applied_member.input_fingerprint != member.fingerprint
                || applied_member.resulting_fingerprint.is_empty()
                || applied_member.result.is_null()
                || assessment.input_fingerprint != applied_member.input_fingerprint
                || assessment.resulting_fingerprint != applied_member.resulting_fingerprint
                || assessment.ready != applied_member.ready
                || assessment.receipt_id != active.id
                || !applied.insert(applied_member.member_key.clone())
            {
                return Err(invalid());
            }
            expected_assessments.insert(applied_member.member_key.clone(), assessment.clone());
        }

        if expected_assessments != new.assessed || !every_member_retired(active, new, &applied) {
            return Err(invalid());
        }
    } else if old.assessed != new.assessed {
        return Err(invalid());
    }

    Ok(())
}

/// Whether every member of `active` outside `except` now holds the exhausted
/// failed record of this very attempt: the only way an attempt clears the
/// slot without certifying a member.
fn every_member_retired(
    active: &MemberAttempt,
    new: &MemberState,
    except: &BTreeSet<String>,
) -> bool {
    active
        .members()
        .iter()
        .filter(|member| !except.contains(&member.key))
        .all(|member| {
            new.failed.get(&member.key).is_some_and(|retired| {
                retired.exhausted
                    && retired.id == active.id
                    && retired.members() == active.members()
                    && retired.attempt == active.attempt
                    && retired.deadline == active.deadline
            })
        })
}
