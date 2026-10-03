use super::*;
use crate::members::preparation::MaterialEvidence;
use chrono::Duration;
use orbit_store::{Store, compose, contracts::AutomationStoreBackend};
use std::{cell::RefCell, collections::BTreeSet};

mod admission;
mod evaluate;
mod incidents;
mod observe;
mod preparation;
mod reconcile;

struct Host {
    fingerprint: RefCell<String>,
    actions: RefCell<BTreeMap<String, String>>,
    evidence: RefCell<Option<MemberEvidence>>,
    failed: RefCell<bool>,
    deferral: RefCell<Option<String>>,
    lose_ack: RefCell<bool>,
}

impl Host {
    fn new() -> Self {
        Self {
            fingerprint: RefCell::new("input".into()),
            actions: RefCell::new(BTreeMap::new()),
            evidence: RefCell::new(None),
            failed: RefCell::new(false),
            deferral: RefCell::new(None),
            lose_ack: RefCell::new(false),
        }
    }
}

impl MemberHost for Host {
    fn head(&self, _: &str) -> Result<(String, SourceRevision), AutomationError> {
        Ok((
            "repo".into(),
            SourceRevision {
                commit: "source".into(),
                tree: "tree".into(),
            },
        ))
    }

    fn observe(&self, _: Option<&str>, now: DateTime<Utc>) -> Result<MemberPage, AutomationError> {
        Ok(MemberPage {
            candidates: vec![StateMember {
                key: "task".into(),
                task_ids: vec!["task".into()],
                fingerprint: self.fingerprint.borrow().clone(),
                source: self.head("")?.1,
                evidence: serde_json::json!({}),
                first_seen: now,
                changed_at: now,
                crew: None,
            }],
            withheld: BTreeMap::new(),
            next: None,
        })
    }

    fn admission(&self, _: &StateMember) -> Result<MemberAdmission, AutomationError> {
        Ok(self
            .deferral
            .borrow()
            .clone()
            .map(MemberAdmission::Withhold)
            .unwrap_or(MemberAdmission::Admit))
    }

    fn observable(&self, keys: &BTreeSet<String>) -> Result<BTreeSet<String>, AutomationError> {
        Ok(keys.clone())
    }

    fn lookup(&self, attempt: &MemberAttempt) -> Result<Option<String>, AutomationError> {
        Ok(self.actions.borrow().get(&attempt.action_key).cloned())
    }

    fn admit(&self, attempt: &MemberAttempt) -> Result<String, AutomationError> {
        let id = self
            .actions
            .borrow_mut()
            .entry(attempt.action_key.clone())
            .or_insert_with(|| format!("run-{}", attempt.attempt))
            .clone();
        if *self.lose_ack.borrow() {
            return Err(AutomationError::Deferred("lost_acknowledgement".into()));
        }
        Ok(id)
    }

    fn outcome(&self, _: &MemberAttempt) -> Result<MemberOutcome, AutomationError> {
        if let Some(evidence) = self.evidence.borrow().clone() {
            Ok(MemberOutcome::Settled(MemberBatchEvidence {
                action_id: evidence.action_id.clone(),
                attempt_id: evidence.attempt_id.clone(),
                applied: vec![evidence],
                failed: BTreeMap::new(),
            }))
        } else if *self.failed.borrow() {
            Ok(MemberOutcome::Failed("fixture_failure".into()))
        } else {
            Ok(MemberOutcome::Pending)
        }
    }
}

struct SchedulerHost {
    candidates: RefCell<Vec<StateMember>>,
    withheld: RefCell<BTreeMap<String, String>>,
    retired: RefCell<BTreeSet<String>>,
    admission_checks: RefCell<Vec<String>>,
    /// Task ids of every admitted attempt, one entry per run.
    admitted: RefCell<Vec<Vec<String>>>,
    settled: RefCell<Option<MemberBatchEvidence>>,
    /// Member keys whose earlier-contract assessment the host vouches for.
    carried: RefCell<BTreeSet<String>>,
}

impl SchedulerHost {
    fn new(candidates: Vec<StateMember>) -> Self {
        Self {
            candidates: RefCell::new(candidates),
            withheld: RefCell::new(BTreeMap::new()),
            retired: RefCell::new(BTreeSet::new()),
            admission_checks: RefCell::new(Vec::new()),
            admitted: RefCell::new(Vec::new()),
            settled: RefCell::new(None),
            carried: RefCell::new(BTreeSet::new()),
        }
    }
}

impl MemberHost for SchedulerHost {
    fn head(&self, _: &str) -> Result<(String, SourceRevision), AutomationError> {
        Ok(("repo".into(), source()))
    }

    fn observe(&self, _: Option<&str>, _: DateTime<Utc>) -> Result<MemberPage, AutomationError> {
        Ok(MemberPage {
            candidates: self.candidates.borrow().clone(),
            withheld: self.withheld.borrow().clone(),
            next: None,
        })
    }

    fn admission(&self, member: &StateMember) -> Result<MemberAdmission, AutomationError> {
        self.admission_checks.borrow_mut().push(member.key.clone());
        if self.retired.borrow().contains(&member.key) {
            Ok(MemberAdmission::Retire("incident_changed".into()))
        } else {
            Ok(MemberAdmission::Admit)
        }
    }

    fn observable(&self, keys: &BTreeSet<String>) -> Result<BTreeSet<String>, AutomationError> {
        Ok(keys.clone())
    }

    fn lookup(&self, _: &MemberAttempt) -> Result<Option<String>, AutomationError> {
        Ok(None)
    }

    fn admit(&self, attempt: &MemberAttempt) -> Result<String, AutomationError> {
        self.admitted.borrow_mut().push(attempt.task_ids());
        Ok(format!("run-{}", attempt.member.key))
    }

    fn outcome(&self, _: &MemberAttempt) -> Result<MemberOutcome, AutomationError> {
        Ok(self
            .settled
            .borrow()
            .clone()
            .map(MemberOutcome::Settled)
            .unwrap_or(MemberOutcome::Pending))
    }

    fn carries_forward(&self, member: &StateMember, _: &MemberAssessment) -> bool {
        self.carried.borrow().contains(&member.key)
    }
}

/// A source paged fifty keys at a time in key order, where each key maps to
/// its current fingerprint and every run settles its members unchanged.
struct RetentionHost {
    source: RefCell<BTreeMap<String, String>>,
    admitted: RefCell<Vec<MemberAttempt>>,
    observable_queries: RefCell<usize>,
}

impl RetentionHost {
    fn new(keys: impl IntoIterator<Item = String>) -> Self {
        Self {
            source: RefCell::new(keys.into_iter().map(|key| (key.clone(), key)).collect()),
            admitted: RefCell::new(Vec::new()),
            observable_queries: RefCell::new(0),
        }
    }
}

impl MemberHost for RetentionHost {
    fn head(&self, _: &str) -> Result<(String, SourceRevision), AutomationError> {
        Ok(("repo".into(), source()))
    }

    fn observe(
        &self,
        after: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<MemberPage, AutomationError> {
        let listed = self.source.borrow();
        let mut rest = listed
            .iter()
            .filter(|(key, _)| after.is_none_or(|after| key.as_str() > after));
        let page = rest.by_ref().take(50).collect::<Vec<_>>();
        let next = rest
            .next()
            .and_then(|_| page.last().map(|(key, _)| (*key).clone()));
        Ok(MemberPage {
            candidates: page
                .into_iter()
                .map(|(key, fingerprint)| StateMember {
                    key: key.clone(),
                    task_ids: vec![key.clone()],
                    fingerprint: fingerprint.clone(),
                    source: source(),
                    evidence: serde_json::json!({}),
                    first_seen: now,
                    changed_at: now,
                    crew: None,
                })
                .collect(),
            withheld: BTreeMap::new(),
            next,
        })
    }

    fn admission(&self, member: &StateMember) -> Result<MemberAdmission, AutomationError> {
        Ok(
            if self.source.borrow().get(&member.key) == Some(&member.fingerprint) {
                MemberAdmission::Admit
            } else {
                MemberAdmission::Retire("material_changed".into())
            },
        )
    }

    fn observable(&self, keys: &BTreeSet<String>) -> Result<BTreeSet<String>, AutomationError> {
        *self.observable_queries.borrow_mut() += 1;
        let listed = self.source.borrow();
        Ok(keys
            .iter()
            .filter(|key| listed.contains_key(*key))
            .cloned()
            .collect())
    }

    fn lookup(&self, _: &MemberAttempt) -> Result<Option<String>, AutomationError> {
        Ok(None)
    }

    fn admit(&self, attempt: &MemberAttempt) -> Result<String, AutomationError> {
        self.admitted.borrow_mut().push(attempt.clone());
        Ok(format!("run-{}", attempt.id))
    }

    fn outcome(&self, attempt: &MemberAttempt) -> Result<MemberOutcome, AutomationError> {
        let Some(action_id) = attempt.action_id.clone() else {
            return Ok(MemberOutcome::Pending);
        };
        Ok(MemberOutcome::Settled(MemberBatchEvidence {
            applied: attempt
                .members()
                .iter()
                .map(|member| MemberEvidence {
                    action_id: action_id.clone(),
                    attempt_id: attempt.id.clone(),
                    member_key: member.key.clone(),
                    input_fingerprint: member.fingerprint.clone(),
                    resulting_fingerprint: member.fingerprint.clone(),
                    ready: true,
                    result: serde_json::json!({"task_id": member.key}),
                })
                .collect(),
            action_id,
            attempt_id: attempt.id.clone(),
            failed: BTreeMap::new(),
        }))
    }
}

fn source() -> SourceRevision {
    SourceRevision {
        commit: "source".into(),
        tree: "tree".into(),
    }
}

fn state_member(key: &str, task_ids: &[&str], first_seen_minute: i64) -> StateMember {
    state_member_with_crew(key, task_ids, first_seen_minute, None)
}

fn state_member_with_crew(
    key: &str,
    task_ids: &[&str],
    first_seen_minute: i64,
    crew: Option<&str>,
) -> StateMember {
    StateMember {
        key: key.into(),
        task_ids: task_ids.iter().map(|id| (*id).into()).collect(),
        fingerprint: key.into(),
        source: source(),
        evidence: serde_json::json!({}),
        first_seen: DateTime::from_timestamp(1_700_000_000, 0).unwrap()
            + Duration::minutes(first_seen_minute),
        changed_at: DateTime::from_timestamp(1_700_000_000, 0).unwrap()
            + Duration::minutes(first_seen_minute),
        crew: crew.map(str::to_string),
    }
}

fn execution_tick(
    store: &dyn AutomationStoreBackend,
    host: &SchedulerHost,
    minute: i64,
) -> AutomationDiagnostic {
    let trigger = StateTrigger {
        kind: StateTriggerKind::ExecutionFailed,
        max_items: 2,
        ..trigger()
    };

    evaluate(
        store,
        host,
        MemberEvaluation {
            consumer: "host/ws/routine/recovery",
            epoch: "epoch",
            trigger: &trigger,
            enabled: true,
            dry_run: false,
            now: DateTime::from_timestamp(1_700_000_000, 0).unwrap() + Duration::minutes(minute),
        },
    )
    .unwrap()
}

fn trigger() -> StateTrigger {
    StateTrigger {
        kind: StateTriggerKind::PreparationEligible,
        owner_machine: "host".into(),
        branch: "agent-main".into(),
        debounce_minutes: 2,
        max_wait_minutes: 10,
        max_items: 50,
        retries: 1,
        deadline_minutes: 30,
        batch_size: None,
        eligibility: PreparationEligibility::default(),
        freshness: Default::default(),
    }
}

fn preparation_tick(
    store: &dyn AutomationStoreBackend,
    host: &SchedulerHost,
    minute: i64,
    dry_run: bool,
) -> AutomationDiagnostic {
    evaluate(
        store,
        host,
        MemberEvaluation {
            consumer: "host/ws/routine/pilot",
            epoch: "epoch",
            trigger: &trigger(),
            enabled: true,
            dry_run,
            now: DateTime::from_timestamp(1_700_000_000, 0).unwrap() + Duration::minutes(minute),
        },
    )
    .unwrap()
}

fn evidence_for(attempt: &MemberAttempt, key: &str, resulting: &str) -> MemberEvidence {
    MemberEvidence {
        action_id: attempt.action_id.clone().unwrap(),
        attempt_id: attempt.id.clone(),
        member_key: key.into(),
        input_fingerprint: attempt.member_for(key).unwrap().fingerprint.clone(),
        resulting_fingerprint: resulting.into(),
        ready: true,
        result: serde_json::json!({"task_id": key}),
    }
}

fn tick(store: &dyn AutomationStoreBackend, host: &Host, minute: i64) -> AutomationDiagnostic {
    evaluate(
        store,
        host,
        MemberEvaluation {
            consumer: "host/ws/routine/pilot",
            epoch: "epoch",
            trigger: &trigger(),
            enabled: true,
            dry_run: false,
            now: DateTime::from_timestamp(1_700_000_000, 0).unwrap() + Duration::minutes(minute),
        },
    )
    .unwrap()
}

fn retention_tick(
    store: &dyn AutomationStoreBackend,
    host: &RetentionHost,
    minute: i64,
) -> AutomationDiagnostic {
    evaluate(
        store,
        host,
        MemberEvaluation {
            consumer: "host/ws/routine/pilot",
            epoch: "epoch",
            trigger: &StateTrigger {
                batch_size: Some(50),
                ..trigger()
            },
            enabled: true,
            dry_run: false,
            now: DateTime::from_timestamp(1_700_000_000, 0).unwrap() + Duration::minutes(minute),
        },
    )
    .unwrap()
}

fn retained_members(diagnostic: &AutomationDiagnostic) -> MemberState {
    diagnostic.state.clone().unwrap().members.unwrap()
}

/// Tick until every source member is assessed and nothing is in flight.
fn assess_source(store: &dyn AutomationStoreBackend, host: &RetentionHost, minute: &mut i64) {
    for _ in 0..200 {
        *minute += 3;
        let members = retained_members(&retention_tick(store, host, *minute));
        if members.active.is_none()
            && members.pending.is_empty()
            && members.assessed.len() == host.source.borrow().len()
        {
            return;
        }
    }
    panic!("the source never settled");
}
