//! State-member invariants use the same transaction and receipt tables.
use orbit_common::OrbitError;
use orbit_types::workflow::automation::{
    AcceptedCoverage, AutomationState, members::MemberEvidence,
};
use sha2::{Digest, Sha256};

pub(super) fn validate(
    previous: &AutomationState,
    next: &AutomationState,
    receipt: Option<&AcceptedCoverage>,
) -> Result<(), OrbitError> {
    let invalid = || OrbitError::InvalidInput("invalid member checkpoint transition".into());
    let old = previous.members.as_ref().ok_or_else(invalid)?;
    let new = next.members.as_ref().ok_or_else(invalid)?;
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
    for (key, failed) in &old.failed {
        if new.failed.get(key) != Some(failed)
            && !old.active.as_ref().is_some_and(|active| {
                &active.member.key == key
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
    if let Some(active) = &old.active {
        if let Some(updated) = &new.active {
            if active.consumer != updated.consumer
                || active.kind != updated.kind
                || active.id != updated.id
                || active.member != updated.member
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
        } else if receipt.is_none()
            && !new.failed.get(&active.member.key).is_some_and(|a| {
                a.exhausted
                    && a.id == active.id
                    && a.member == active.member
                    && a.attempt == active.attempt
                    && a.deadline == active.deadline
            })
        {
            return Err(invalid());
        }
    }
    if old.active.is_none()
        && let Some(active) = &new.active
        && (active.consumer != previous.consumer
            || active.attempt != 1
            || active.max_attempts == 0
            || active.max_attempts > 6
            || !new.pending.values().any(|m| m == &active.member)
            || active.action_id.is_some()
            || active.exhausted
            || active.deadline <= active.retry_after)
    {
        return Err(invalid());
    }
    if let Some(receipt) = receipt {
        let active = old.active.as_ref().ok_or_else(invalid)?;
        let evidence: MemberEvidence =
            serde_json::from_slice(&receipt.evidence).map_err(|_| invalid())?;
        let bytes = serde_json::to_vec(active).map_err(|_| invalid())?;
        if receipt.batch_id != active.id
            || active.action_id.as_ref() != Some(&receipt.action_id)
            || receipt.input_digest != format!("{:x}", Sha256::digest(bytes))
            || receipt.evidence_digest != format!("{:x}", Sha256::digest(&receipt.evidence))
            || evidence.action_id != receipt.action_id
            || evidence.attempt_id != active.id
            || evidence.member_key != active.member.key
            || evidence.input_fingerprint != active.member.fingerprint
            || evidence.resulting_fingerprint.is_empty()
            || evidence.result.is_null()
            || new.active.is_some()
            || receipt.submitted_by != "system"
        {
            return Err(invalid());
        }
        let mut expected_assessments = old.assessed.clone();
        expected_assessments.insert(
            active.member.key.clone(),
            new.assessed
                .get(&active.member.key)
                .ok_or_else(invalid)?
                .clone(),
        );
        if expected_assessments != new.assessed {
            return Err(invalid());
        }
        let assessment = new.assessed.get(&active.member.key).ok_or_else(invalid)?;
        if assessment.input_fingerprint != evidence.input_fingerprint
            || assessment.resulting_fingerprint != evidence.resulting_fingerprint
            || assessment.ready != evidence.ready
            || assessment.receipt_id != active.id
        {
            return Err(invalid());
        }
    } else if old.assessed != new.assessed {
        return Err(invalid());
    }
    Ok(())
}
