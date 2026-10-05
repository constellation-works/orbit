//! Settle the in-flight member attempt against Core's outcome.

use super::admission::{fit_failed, retire_attempt};
use super::evaluate::member_state;
use super::{MemberAdmission, MemberHost, MemberOutcome};
use crate::AutomationError;
use crate::checkpoint::commit;
use crate::delivery::digest;
use chrono::{DateTime, Duration, Utc};
use orbit_store::contracts::AutomationStoreBackend;
use orbit_types::workflow::automation::{members::*, *};
use std::collections::BTreeSet;

pub(super) fn reconcile(
    store: &dyn AutomationStoreBackend,
    host: &dyn MemberHost,
    state: AutomationState,
    now: DateTime<Utc>,
) -> Result<AutomationState, AutomationError> {
    let Some(active) = state
        .members
        .as_ref()
        .and_then(|members| members.active.as_ref())
    else {
        return Ok(state);
    };

    if active.exhausted {
        return Ok(state);
    }

    // Before an action id exists the claim is unresolved: adopt one Core already
    // created, otherwise retire the claim once its input or deadline went stale.
    if active.action_id.is_none() {
        if let Some(id) = host.lookup(active)? {
            let mut next = state.clone();
            if let Some(active) = &mut member_state(&mut next)?.active {
                active.action_id = Some(id);
            }

            return commit(store, &state, next, None);
        }

        if now >= active.deadline {
            let mut next = state.clone();
            let members = member_state(&mut next)?;
            if let Some(expired) = members.active.take() {
                for member in expired.members() {
                    members
                        .withheld
                        .insert(member.key.clone(), "input_stale_or_deadline_expired".into());
                }
                retire_attempt(host, members, expired)?;
            }

            return commit(store, &state, next, None);
        }

        // A member whose input went stale leaves the batch: a retired identity
        // is dropped for re-observation, a withheld one stays pending. The rest
        // keep the attempt; only an emptied batch retires the claim.
        let mut kept = Vec::new();
        let mut next = state.clone();
        for member in active.members() {
            match host.admission(member)? {
                MemberAdmission::Admit => kept.push(member.clone()),
                MemberAdmission::Withhold(reason) => {
                    member_state(&mut next)?
                        .withheld
                        .insert(member.key.clone(), reason);
                }
                MemberAdmission::Retire(_) => {
                    let members = member_state(&mut next)?;
                    members.pending.remove(&member.key);
                    members.withheld.remove(&member.key);
                }
            }
        }

        if kept.len() == active.members().len() {
            return Ok(state);
        }

        let members = member_state(&mut next)?;
        let Some(mut shrunk) = members.active.take() else {
            return Ok(state);
        };
        if let Some(first) = kept.first().cloned() {
            shrunk.member = first;
            shrunk.members = kept;
            members.active = Some(shrunk);
        } else {
            for member in shrunk.members() {
                if members.pending.contains_key(&member.key) {
                    members
                        .withheld
                        .insert(member.key.clone(), "input_stale_or_deadline_expired".into());
                }
            }
            retire_attempt(host, members, shrunk)?;
        }

        return commit(store, &state, next, None);
    }

    match host.outcome(active)? {
        MemberOutcome::Pending => Ok(state),
        MemberOutcome::Settled(evidence) => {
            let mut applied_keys = BTreeSet::new();
            if Some(&evidence.action_id) != active.action_id.as_ref()
                || evidence.attempt_id != active.id
                || evidence.applied.iter().any(|applied| {
                    applied.action_id != evidence.action_id
                        || applied.attempt_id != evidence.attempt_id
                        || active
                            .member_for(&applied.member_key)
                            .is_none_or(|member| member.fingerprint != applied.input_fingerprint)
                        || applied.resulting_fingerprint.is_empty()
                        || applied.result.is_null()
                        || !applied_keys.insert(applied.member_key.clone())
                })
            {
                return Err(AutomationError::Evidence(
                    "member_provenance_mismatch".into(),
                ));
            }

            let bytes = serde_json::to_vec(&evidence)
                .map_err(|e| AutomationError::Evidence(e.to_string()))?;

            // One receipt certifies every member the run applied; members it
            // did not apply are failed at their fingerprint beside it.
            let input_digest = digest(
                &active
                    .identity_bytes()
                    .map_err(|e| AutomationError::Evidence(e.to_string()))?,
            );
            let receipt = (!evidence.applied.is_empty()).then(|| AcceptedCoverage {
                batch_id: active.id.clone(),
                action_id: evidence.action_id.clone(),
                input_digest,
                evidence_digest: digest(&bytes),
                evidence: bytes,
                evidence_reference: format!(
                    "run:{}/deterministic-apply/{}",
                    evidence.action_id,
                    applied_keys.iter().cloned().collect::<Vec<_>>().join(",")
                ),
                submitted_by: "system".into(),
                accepted_at: now,
            });

            let mut next = state.clone();
            let members = member_state(&mut next)?;
            for applied in &evidence.applied {
                members.assessed.insert(
                    applied.member_key.clone(),
                    MemberAssessment {
                        input_fingerprint: applied.input_fingerprint.clone(),
                        resulting_fingerprint: applied.resulting_fingerprint.clone(),
                        ready: applied.ready,
                        receipt_id: active.id.clone(),
                    },
                );

                // Only drop the pending entry the evidence actually covers; a newer
                // fingerprint arrived while this attempt ran and still needs applying.
                if members
                    .pending
                    .get(&applied.member_key)
                    .is_some_and(|pending| pending.fingerprint == applied.input_fingerprint)
                {
                    members.pending.remove(&applied.member_key);
                }
            }

            let Some(settled) = members.active.take() else {
                return Ok(state);
            };
            for member in settled.members() {
                if applied_keys.contains(&member.key) {
                    continue;
                }
                let reason = evidence
                    .failed
                    .get(&member.key)
                    .cloned()
                    .unwrap_or_else(|| "no_member_evidence".into());
                members.withheld.insert(member.key.clone(), reason);
                if let Some(record) = settled.failure_record(&member.key) {
                    members.failed.insert(member.key.clone(), record);
                }
            }
            fit_failed(host, members, &settled)?;

            commit(store, &state, next, receipt.as_ref())
        }
        MemberOutcome::Failed(reason) => {
            let mut next = state.clone();
            let members = member_state(&mut next)?;
            for member in active.members() {
                members.withheld.insert(member.key.clone(), reason.clone());
            }

            if let Some(active) = &mut members.active {
                let retry_budget_remains =
                    active.attempt < active.max_attempts && now < active.deadline;

                if retry_budget_remains {
                    active.attempt += 1;
                    active.action_id = None;
                    active.action_key = format!("automation:{}:{}", active.id, active.attempt);
                    active.retry_after = now + Duration::minutes(5);
                } else {
                    active.exhausted = true;
                }
            }

            if members
                .active
                .as_ref()
                .is_some_and(|active| active.exhausted)
                && let Some(exhausted) = members.active.take()
            {
                retire_attempt(host, members, exhausted)?;
            }

            commit(store, &state, next, None)
        }
    }
}
