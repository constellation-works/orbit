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
    /// Only deterministic apply output with host-verified origin is admissible.
    Applied(MemberEvidence),
    /// Failed requires proof the owner and all applicable recoveries stopped.
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
    /// Resolved operation-mode scheduling preferences and admission scope
    /// supplied by Core [ORB-11332]. Empty constraints leave the trigger's
    /// own timing untouched.
    pub constraints: MemberConstraints,
}

/// Operation-mode inputs to the shared due decision [ORB-11332].
///
/// Core resolves preferences and the active grant; this evaluator only applies
/// them. A member inside `scope` becomes due once it has settled for
/// `due_after_seconds`, in addition to the trigger's own debounce/max-wait
/// rule. Members outside the scope, and every member when the scope is
/// empty, keep the operator's routine timing unchanged, so a grant can only
/// accelerate the work it names and never gates an independently enabled
/// routine. Constraints never grant authority: admission still goes through
/// [`MemberHost::admission`] and the pipeline's own checks.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemberConstraints {
    /// Task ids the active grant covers.
    pub scope: BTreeSet<String>,
    /// Seconds after the last material change before an in-scope member is
    /// due. `None` applies no acceleration.
    pub due_after_seconds: Option<u64>,
}

impl MemberConstraints {
    /// Whether the constraints accelerate `member`.
    fn accelerates(&self, member: &StateMember, now: DateTime<Utc>) -> bool {
        let Some(due_after) = self.due_after_seconds else {
            return false;
        };
        member.task_ids.iter().any(|id| self.scope.contains(id))
            && now.signed_duration_since(member.changed_at).num_seconds()
                >= i64::try_from(due_after).unwrap_or(i64::MAX)
    }
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
        constraints,
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
        if active.action_id.is_some() {
            return diagnostic(store, consumer, "batch_pending", Some(state));
        }

        if now >= active.deadline {
            return diagnostic(store, consumer, "retry_deadline_expired", Some(state));
        }

        if now < active.retry_after {
            return diagnostic(store, consumer, "retry_backoff", Some(state));
        }

        return admit(store, host, state, dry_run);
    }

    // A member is due once it has settled for the debounce window, or waited out
    // the maximum; a member that already failed at this fingerprint is not retried.
    let mut candidates = members
        .pending
        .values()
        .filter(|member| {
            if members
                .failed
                .get(&member.key)
                .is_some_and(|failed| failed.member.fingerprint == member.fingerprint)
            {
                return false;
            }

            now.signed_duration_since(member.changed_at).num_minutes()
                >= i64::from(trigger.debounce_minutes)
                || now.signed_duration_since(member.first_seen).num_minutes()
                    >= i64::from(trigger.max_wait_minutes)
                || constraints.accelerates(member, now)
        })
        .cloned()
        .collect::<Vec<_>>();

    candidates.sort_by(|a, b| a.first_seen.cmp(&b.first_seen).then(a.key.cmp(&b.key)));

    if candidates.is_empty() {
        let reason = if members.failed.iter().any(|(key, failed)| {
            members
                .pending
                .get(key)
                .is_some_and(|pending| pending.fingerprint == failed.member.fingerprint)
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

    // Take the first due member Core will admit. Temporary refusals remain
    // visible, while obsolete source identities are retired from durable state.
    let mut candidate = None;
    let mut next = state.clone();

    for member in candidates.into_iter().take(trigger.max_items) {
        match host.admission(&member)? {
            MemberAdmission::Admit => {
                candidate = Some(member);
                break;
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

    let Some(member) = candidate else {
        return diagnostic(store, consumer, "work_withheld", Some(state));
    };

    if dry_run {
        return diagnostic(store, consumer, "would_fire", Some(state));
    }

    let id = definition_epoch(&(consumer, epoch, &member, now))?;
    let attempt = MemberAttempt {
        consumer: consumer.into(),
        kind: trigger.kind,
        action_key: format!("automation:{id}:1"),
        id,
        member,
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

    admit(store, host, state, false)
}

fn member_state(state: &mut AutomationState) -> Result<&mut MemberState, AutomationError> {
    state
        .members
        .as_mut()
        .ok_or_else(|| AutomationError::Deferred("state_missing".into()))
}

fn admit(
    store: &dyn AutomationStoreBackend,
    host: &dyn MemberHost,
    state: AutomationState,
    dry_run: bool,
) -> Result<AutomationDiagnostic, AutomationError> {
    let consumer = state.consumer.clone();
    let active = state
        .members
        .as_ref()
        .and_then(|members| members.active.as_ref())
        .ok_or_else(|| AutomationError::Deferred("claim_missing".into()))?;

    match host.admission(&active.member)? {
        MemberAdmission::Admit => {}
        MemberAdmission::Withhold(reason) | MemberAdmission::Retire(reason) => {
            return diagnostic(store, &consumer, &reason, Some(state));
        }
    }

    if dry_run {
        return diagnostic(store, &consumer, "would_fire", Some(state));
    }

    let id = host.admit(active)?;

    let mut next = state.clone();
    if let Some(active) = &mut member_state(&mut next)?.active {
        active.action_id = Some(id);
    }

    let next = commit(store, &state, next, None)?;

    diagnostic(store, &consumer, "fired", Some(next))
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

        let admission = host.admission(&active.member)?;
        if now >= active.deadline || !matches!(&admission, MemberAdmission::Admit) {
            let mut next = state.clone();
            let members = member_state(&mut next)?;
            if let Some(mut expired) = members.active.take() {
                expired.exhausted = true;
                if matches!(&admission, MemberAdmission::Retire(_)) {
                    members.pending.remove(&expired.member.key);
                    members.withheld.remove(&expired.member.key);
                } else {
                    members.withheld.insert(
                        expired.member.key.clone(),
                        "input_stale_or_deadline_expired".into(),
                    );
                }
                members.failed.insert(expired.member.key.clone(), expired);
            }

            return commit(store, &state, next, None);
        }

        return Ok(state);
    }

    match host.outcome(active)? {
        MemberOutcome::Pending => Ok(state),
        MemberOutcome::Applied(evidence) => {
            if Some(&evidence.action_id) != active.action_id.as_ref()
                || evidence.attempt_id != active.id
                || evidence.member_key != active.member.key
                || evidence.input_fingerprint != active.member.fingerprint
                || evidence.resulting_fingerprint.is_empty()
                || evidence.result.is_null()
            {
                return Err(AutomationError::Evidence(
                    "member_provenance_mismatch".into(),
                ));
            }

            let bytes = serde_json::to_vec(&evidence)
                .map_err(|e| AutomationError::Evidence(e.to_string()))?;

            let receipt = AcceptedCoverage {
                batch_id: active.id.clone(),
                action_id: evidence.action_id.clone(),
                input_digest: definition_epoch(active)?,
                evidence_digest: digest(&bytes),
                evidence: bytes,
                evidence_reference: format!(
                    "run:{}/deterministic-apply/{}",
                    evidence.action_id, active.member.key
                ),
                submitted_by: "system".into(),
                accepted_at: now,
            };

            let mut next = state.clone();
            let members = member_state(&mut next)?;
            members.assessed.insert(
                active.member.key.clone(),
                MemberAssessment {
                    input_fingerprint: evidence.input_fingerprint,
                    resulting_fingerprint: evidence.resulting_fingerprint,
                    ready: evidence.ready,
                    receipt_id: receipt.batch_id.clone(),
                },
            );

            // Only drop the pending entry the evidence actually covers; a newer
            // fingerprint arrived while this attempt ran and still needs applying.
            if members
                .pending
                .get(&active.member.key)
                .is_some_and(|pending| pending.fingerprint == active.member.fingerprint)
            {
                members.pending.remove(&active.member.key);
            }

            members.active = None;

            commit(store, &state, next, Some(&receipt))
        }
        MemberOutcome::Failed(reason) => {
            let mut next = state.clone();
            let members = member_state(&mut next)?;
            members.withheld.insert(active.member.key.clone(), reason);

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
                members
                    .failed
                    .insert(exhausted.member.key.clone(), exhausted);
            }

            commit(store, &state, next, None)
        }
    }
}
