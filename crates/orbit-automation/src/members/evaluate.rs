//! One member evaluation pass: reconcile, absorb a page, then admit due members.

use super::admission::admit;
use super::observe::{absorb, retire_unobserved};
use super::reconcile::reconcile;
use super::{MemberAdmission, MemberEvaluation, MemberHost};
use crate::AutomationError;
use crate::checkpoint::{commit, diagnostic};
use crate::delivery::definition_epoch;
use chrono::{DateTime, Duration, Utc};
use orbit_store::contracts::AutomationStoreBackend;
use orbit_types::workflow::automation::{members::*, *};
use std::collections::BTreeMap;

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
pub(super) fn active_batch(attempt: &MemberAttempt) -> Vec<BatchMember> {
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

pub(super) fn diagnostic_with_batch(
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

    if page.candidates.len() + page.withheld.len() > 50
        || page.candidates.iter().any(|member| {
            member.key.is_empty() || member.fingerprint.is_empty() || member.task_ids.is_empty()
        })
    {
        return Err(AutomationError::Deferred("source_page_invalid".into()));
    }

    // A page that does not fit first retires what departed members left
    // behind; whatever still does not fit waits for a later pass, while the
    // scan and the retained members keep moving.
    if absorb(host, &mut members.clone(), &page) > 0 {
        retire_unobserved(host, members, &page)?;
    }
    let deferred = absorb(host, members, &page);
    members.scan_after = page.next;

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
        let reason = if deferred > 0 {
            "source_backpressure"
        } else if members.failed.iter().any(|(key, failed)| {
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

pub(super) fn member_state(
    state: &mut AutomationState,
) -> Result<&mut MemberState, AutomationError> {
    state
        .members
        .as_mut()
        .ok_or_else(|| AutomationError::Deferred("state_missing".into()))
}
