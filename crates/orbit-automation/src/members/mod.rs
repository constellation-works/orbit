//! State consumers share the sweep, generation-fenced Store and receipt path.

use crate::AutomationError;
use crate::checkpoint::{commit, diagnostic};
use crate::delivery::{definition_epoch, digest};
use chrono::{DateTime, Duration, Utc};
use orbit_store::contracts::AutomationStoreBackend;
use orbit_types::workflow::automation::{members::*, *};
use std::collections::{BTreeMap, BTreeSet};

pub mod incidents;
pub mod preparation;
#[cfg(test)]
mod tests;

pub struct MemberPage {
    pub candidates: Vec<StateMember>,
    pub withheld: BTreeMap<String, String>,
    pub next: Option<String>,
}

pub enum MemberOutcome {
    Pending,
    /// The run's deterministic apply settled every member of the attempt:
    /// only apply output with host-verified origin is admissible, and a member
    /// it did not apply is failed at its fingerprint without retry.
    Settled(MemberBatchEvidence),
    /// The run stopped before any apply output existed; the attempt retries
    /// as a whole while its budget lasts. Requires proof the owner and all
    /// applicable recoveries stopped.
    Failed(String),
}

pub enum MemberAdmission {
    Admit,
    /// The member is still authoritative, but cannot be admitted yet.
    Withhold(String),
    /// The member no longer describes authoritative source state.
    Retire(String),
}

pub trait MemberHost {
    fn head(&self, branch: &str) -> Result<(String, SourceRevision), AutomationError>;

    fn observe(
        &self,
        after: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<MemberPage, AutomationError>;

    fn admission(&self, member: &StateMember) -> Result<MemberAdmission, AutomationError>;

    fn lookup(&self, attempt: &MemberAttempt) -> Result<Option<String>, AutomationError>;

    fn admit(&self, attempt: &MemberAttempt) -> Result<String, AutomationError>;

    fn outcome(&self, attempt: &MemberAttempt) -> Result<MemberOutcome, AutomationError>;
}

pub struct MemberEvaluation<'a> {
    pub consumer: &'a str,
    pub epoch: &'a str,
    pub trigger: &'a StateTrigger,
    pub enabled: bool,
    pub dry_run: bool,
    pub now: DateTime<Utc>,
}

/// Why a pending member is due now, or `None` while it still debounces: it
/// settled for the debounce window, or it waited out the maximum.
fn due_reason(
    member: &StateMember,
    trigger: &StateTrigger,
    now: DateTime<Utc>,
) -> Option<&'static str> {
    if now.signed_duration_since(member.changed_at).num_minutes()
        >= i64::from(trigger.debounce_minutes)
    {
        Some("settled")
    } else if now.signed_duration_since(member.first_seen).num_minutes()
        >= i64::from(trigger.max_wait_minutes)
    {
        Some("max_wait")
    } else {
        None
    }
}

/// The in-flight batch as diagnostics: every member was due when claimed.
fn active_batch(attempt: &MemberAttempt) -> Vec<BatchMember> {
    attempt
        .members()
        .iter()
        .map(|member| BatchMember {
            key: member.key.clone(),
            task_ids: member.task_ids.clone(),
            reason: "admitted".into(),
        })
        .collect()
}

fn diagnostic_with_batch(
    store: &dyn AutomationStoreBackend,
    consumer: &str,
    reason: &str,
    state: AutomationState,
    batch: Vec<BatchMember>,
) -> Result<AutomationDiagnostic, AutomationError> {
    let mut diagnostic = diagnostic(store, consumer, reason, Some(state))?;
    diagnostic.batch = batch;
    Ok(diagnostic)
}

pub fn evaluate(
    store: &dyn AutomationStoreBackend,
    host: &dyn MemberHost,
    request: MemberEvaluation<'_>,
) -> Result<AutomationDiagnostic, AutomationError> {
    let MemberEvaluation {
        consumer,
        epoch,
        trigger,
        enabled,
        dry_run,
        now,
    } = request;

    trigger.validate().map_err(orbit_common::OrbitError::from)?;

    let mut state = match store.automation_state(consumer)? {
        Some(state) => state,
        None => {
            if !enabled && !dry_run {
                return diagnostic(store, consumer, "disabled", None);
            }

            let (repository, head) = host.head(&trigger.branch)?;
            let state = AutomationState {
                members: Some(MemberState::default()),
                consumer: consumer.into(),
                epoch: epoch.into(),
                // State members carry no delivery trigger identity.
                trigger: None,
                repository,
                branch: trigger.branch.clone(),
                generation: 0,
                baseline: head.clone(),
                observed: head.clone(),
                covered: head,
                pending_commits: vec![],
                pending: vec![],
                waived: vec![],
                excluded: vec![],
                unresolved: BTreeMap::new(),
                associations: BTreeMap::new(),
                active: None,
                stall: None,
            };

            if !dry_run && !store.automation_initialize(&state)? {
                return Err(AutomationError::Deferred("concurrent_evaluation".into()));
            }

            state
        }
    };

    if state.members.is_none() {
        return diagnostic(store, consumer, "definition_changed", Some(state));
    }

    if !dry_run {
        state = reconcile(store, host, state, now)?;
    }

    if state.epoch != epoch || state.branch != trigger.branch {
        return diagnostic(store, consumer, "definition_changed", Some(state));
    }

    if !enabled {
        return diagnostic(store, consumer, "disabled", Some(state));
    }

    // Absorb one observation page into the pending/withheld working set.
    let mut next = state.clone();
    let members = member_state(&mut next)?;
    let page = host.observe(members.scan_after.as_deref(), now)?;

    if page.candidates.len() + page.withheld.len() > 50 {
        return Err(AutomationError::Deferred("source_page_invalid".into()));
    }

    for (key, reason) in page.withheld {
        members.pending.remove(&key);
        members.withheld.insert(key, reason);
    }

    for mut member in page.candidates {
        if member.key.is_empty() || member.fingerprint.is_empty() || member.task_ids.is_empty() {
            return Err(AutomationError::Deferred("source_page_invalid".into()));
        }

        members.withheld.remove(&member.key);
        for task_id in &member.task_ids {
            members.withheld.remove(task_id);
        }

        // Already assessed at exactly this fingerprint: nothing left to apply.
        if members
            .assessed
            .get(&member.key)
            .is_some_and(|assessed| assessed.resulting_fingerprint == member.fingerprint)
        {
            members.pending.remove(&member.key);
            continue;
        }

        // Re-seeing a member preserves how long it has waited, and an unchanged
        // fingerprint also preserves when it last changed.
        if let Some(old) = members.pending.get(&member.key) {
            member.first_seen = old.first_seen;
            if old.fingerprint == member.fingerprint {
                member.changed_at = old.changed_at;
            }
        }

        members.pending.insert(member.key.clone(), member);
    }

    members.scan_after = page.next;

    if members.pending.len() + members.assessed.len() + members.withheld.len() > 1000 {
        return diagnostic(store, consumer, "source_backpressure", Some(state));
    }

    state = if dry_run {
        next
    } else {
        commit(store, &state, next, None)?
    };

    let members = state
        .members
        .as_ref()
        .ok_or_else(|| AutomationError::Deferred("state_missing".into()))?;

    // An attempt already in flight owns the slot; resume or report it, never start another.
    if let Some(active) = &members.active {
        let batch = active_batch(active);
        let reason = if active.action_id.is_some() {
            "batch_pending"
        } else if now >= active.deadline {
            "retry_deadline_expired"
        } else if now < active.retry_after {
            "retry_backoff"
        } else {
            return admit(store, host, state, dry_run, None);
        };

        return diagnostic_with_batch(store, consumer, reason, state, batch);
    }

    // A member is due once it has settled for the debounce window, or waited out
    // the maximum; a member that already failed at this fingerprint is not retried.
    let mut candidates = members
        .pending
        .values()
        .filter(|member| {
            !members.failed.get(&member.key).is_some_and(|failed| {
                failed
                    .member_for(&member.key)
                    .is_some_and(|retired| retired.fingerprint == member.fingerprint)
            })
        })
        .filter_map(|member| {
            due_reason(member, trigger, now).map(|reason| (member.clone(), reason))
        })
        .collect::<Vec<_>>();

    candidates.sort_by(|(a, _), (b, _)| a.first_seen.cmp(&b.first_seen).then(a.key.cmp(&b.key)));

    if candidates.is_empty() {
        let reason = if members.failed.iter().any(|(key, failed)| {
            members.pending.get(key).is_some_and(|pending| {
                failed
                    .member_for(key)
                    .is_some_and(|retired| retired.fingerprint == pending.fingerprint)
            })
        }) {
            "needs_attention"
        } else if !members.pending.is_empty() {
            "debouncing"
        } else if !members.withheld.is_empty() {
            "work_withheld"
        } else if members.assessed.values().any(|assessed| !assessed.ready) {
            "fresh_unready"
        } else {
            "fresh"
        };
        return diagnostic(store, consumer, reason, Some(state));
    }

    // Batch the due members Core will admit, oldest first, up to the batch
    // size and `max_items` admission checks [ORB-12746]. Temporary refusals
    // remain visible, while obsolete source identities are retired from
    // durable state. One attempt pins one source and one crew identity, so a
    // member observed at another head — or carrying a different stored
    // `task.crew` — waits for the next admission rather than mixing the
    // bundle the one-bundle-one-crew dispatch rule would reject [ORB-12761].
    let batch_size = trigger.effective_batch_size();
    let mut admitted = Vec::new();
    let mut batch = Vec::new();
    let mut next = state.clone();

    for (member, reason) in candidates.into_iter().take(trigger.max_items) {
        if admitted.len() >= batch_size {
            break;
        }
        if admitted.first().is_some_and(|first: &StateMember| {
            first.source != member.source || first.crew != member.crew
        }) {
            continue;
        }
        match host.admission(&member)? {
            MemberAdmission::Admit => {
                batch.push(BatchMember {
                    key: member.key.clone(),
                    task_ids: member.task_ids.clone(),
                    reason: reason.into(),
                });
                admitted.push(member);
            }
            MemberAdmission::Withhold(reason) => {
                member_state(&mut next)?.withheld.insert(member.key, reason);
            }
            MemberAdmission::Retire(_) => {
                let members = member_state(&mut next)?;
                members.pending.remove(&member.key);
                members.withheld.remove(&member.key);
            }
        }
    }

    if next != state {
        state = if dry_run {
            next
        } else {
            commit(store, &state, next, None)?
        };
    }

    let Some(member) = admitted.first().cloned() else {
        return diagnostic(store, consumer, "work_withheld", Some(state));
    };

    if dry_run {
        return diagnostic_with_batch(store, consumer, "would_fire", state, batch);
    }

    let id = definition_epoch(&(consumer, epoch, &admitted, now))?;
    let attempt = MemberAttempt {
        consumer: consumer.into(),
        kind: trigger.kind,
        action_key: format!("automation:{id}:1"),
        id,
        member,
        members: admitted,
        attempt: 1,
        max_attempts: trigger.retries + 1,
        deadline: now + Duration::minutes(i64::from(trigger.deadline_minutes)),
        retry_after: now,
        action_id: None,
        exhausted: false,
    };

    let mut next = state.clone();
    member_state(&mut next)?.active = Some(attempt);
    state = commit(store, &state, next, None)?;

    admit(store, host, state, false, Some(batch))
}

fn member_state(state: &mut AutomationState) -> Result<&mut MemberState, AutomationError> {
    state
        .members
        .as_mut()
        .ok_or_else(|| AutomationError::Deferred("state_missing".into()))
}

/// Acknowledge the active attempt with Core. `batch` carries the due reasons
/// of a claim made this pass; a resumed claim reports its members as admitted.
fn admit(
    store: &dyn AutomationStoreBackend,
    host: &dyn MemberHost,
    state: AutomationState,
    dry_run: bool,
    batch: Option<Vec<BatchMember>>,
) -> Result<AutomationDiagnostic, AutomationError> {
    let consumer = state.consumer.clone();
    let active = state
        .members
        .as_ref()
        .and_then(|members| members.active.as_ref())
        .ok_or_else(|| AutomationError::Deferred("claim_missing".into()))?;

    let batch = batch.unwrap_or_else(|| active_batch(active));

    // Every member must still be admissible; `reconcile` shrinks the batch
    // to the ones that are on the next pass rather than failing siblings.
    for member in active.members() {
        match host.admission(member)? {
            MemberAdmission::Admit => {}
            MemberAdmission::Withhold(reason) | MemberAdmission::Retire(reason) => {
                return diagnostic_with_batch(store, &consumer, &reason, state, batch);
            }
        }
    }

    if dry_run {
        return diagnostic_with_batch(store, &consumer, "would_fire", state, batch);
    }

    let id = host.admit(active)?;

    let mut next = state.clone();
    if let Some(active) = &mut member_state(&mut next)?.active {
        active.action_id = Some(id);
    }

    let next = commit(store, &state, next, None)?;

    diagnostic_with_batch(store, &consumer, "fired", next, batch)
}

/// Record `attempt` as the exhausted failed input of every member it still
/// carries, so none of them refires at the same fingerprint.
fn retire_attempt(members: &mut MemberState, mut attempt: MemberAttempt) {
    attempt.exhausted = true;
    for member in attempt.members().to_vec() {
        members.failed.insert(member.key, attempt.clone());
    }
}

fn reconcile(
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
                retire_attempt(members, expired);
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
            retire_attempt(members, shrunk);
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
            let input_digest = definition_epoch(active)?;
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

            let Some(mut settled) = members.active.take() else {
                return Ok(state);
            };
            settled.exhausted = true;
            for member in settled.members().to_vec() {
                if applied_keys.contains(&member.key) {
                    continue;
                }
                let reason = evidence
                    .failed
                    .get(&member.key)
                    .cloned()
                    .unwrap_or_else(|| "no_member_evidence".into());
                members.withheld.insert(member.key.clone(), reason);
                members.failed.insert(member.key, settled.clone());
            }

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
                retire_attempt(members, exhausted);
            }

            commit(store, &state, next, None)
        }
    }
}
