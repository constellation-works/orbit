//! Failed member records stay bounded by what the source still observes.

use super::*;

/// Members each wave admits as one batch, all of which fail.
const WAVE: usize = 10;

/// Task ids each wave's page withholds beside them, filling the working set
/// the way a large backlog does, so departed members are retired regularly.
const WITHHELD: usize = 40;

/// Waves driven: their distinct failures pass the store's 1,000-record cap.
const WAVES: usize = 101;

/// A source whose members each fail their one attempt and then leave it,
/// the way a closed task leaves the statuses a task-pilot queries.
#[derive(Default)]
struct FailingHost {
    candidates: RefCell<BTreeSet<String>>,
    withheld: RefCell<BTreeSet<String>>,
}

impl MemberHost for FailingHost {
    fn head(&self, _: &str) -> Result<(String, SourceRevision), AutomationError> {
        Ok(("fixture-repo".into(), source()))
    }
    fn observe(&self, _: Option<&str>, now: DateTime<Utc>) -> Result<MemberPage, AutomationError> {
        Ok(MemberPage {
            candidates: self
                .candidates
                .borrow()
                .iter()
                .map(|key| StateMember {
                    key: key.clone(),
                    task_ids: vec![key.clone()],
                    fingerprint: "input".into(),
                    source: source(),
                    evidence: serde_json::Value::Null,
                    // Already settled, so a wave is due when first observed.
                    first_seen: now - Duration::minutes(1),
                    changed_at: now - Duration::minutes(1),
                    crew: None,
                })
                .collect(),
            withheld: self
                .withheld
                .borrow()
                .iter()
                .map(|key| (key.clone(), "fixture_blocked".into()))
                .collect(),
            next: None,
        })
    }
    fn admission(&self, _: &StateMember) -> Result<MemberAdmission, AutomationError> {
        Ok(MemberAdmission::Admit)
    }
    fn observable(&self, keys: &BTreeSet<String>) -> Result<BTreeSet<String>, AutomationError> {
        let (candidates, withheld) = (self.candidates.borrow(), self.withheld.borrow());
        Ok(keys
            .iter()
            .filter(|key| candidates.contains(*key) || withheld.contains(*key))
            .cloned()
            .collect())
    }
    fn lookup(&self, _: &MemberAttempt) -> Result<Option<String>, AutomationError> {
        Ok(None)
    }
    fn admit(&self, attempt: &MemberAttempt) -> Result<String, AutomationError> {
        Ok(format!("run-{}", attempt.id))
    }
    fn outcome(&self, attempt: &MemberAttempt) -> Result<MemberOutcome, AutomationError> {
        Ok(MemberOutcome::Settled(MemberBatchEvidence {
            action_id: attempt.action_id.clone().unwrap_or_default(),
            attempt_id: attempt.id.clone(),
            applied: vec![],
            failed: attempt
                .members()
                .iter()
                .map(|member| (member.key.clone(), "fixture_failure".into()))
                .collect(),
        }))
    }
}

fn source() -> SourceRevision {
    SourceRevision {
        commit: "head".into(),
        tree: "tree".into(),
    }
}

fn failing_tick(store: &dyn AutomationStoreBackend, host: &FailingHost, minute: i64) -> String {
    evaluate(
        store,
        host,
        MemberEvaluation {
            consumer: "fixture/member-failures",
            epoch: "fixture-epoch",
            enabled: true,
            dry_run: false,
            now: at("2026-01-01T00:00:00Z") + Duration::minutes(minute),
            trigger: &StateTrigger {
                kind: StateTriggerKind::PreparationEligible,
                owner_machine: "fixture-host".into(),
                branch: "fixture-branch".into(),
                debounce_minutes: 1,
                max_wait_minutes: 10,
                max_items: WAVE,
                retries: 0,
                deadline_minutes: 30,
                batch_size: Some(WAVE),
                eligibility: Default::default(),
                freshness: Default::default(),
            },
        },
    )
    .unwrap_or_else(|error| panic!("minute {minute}: evaluation must commit: {error}"))
    .reason
}

#[test]
fn consumer_keeps_admitting_after_a_thousand_distinct_failed_members() {
    if isolated("failed_members::consumer_keeps_admitting_after_a_thousand_distinct_failed_members")
    {
        return;
    }
    let store = compose::automation_store(Store::open_in_memory().unwrap()).unwrap();
    let host = FailingHost::default();
    let mut failed_keys = BTreeSet::new();

    for wave in 0..WAVES {
        // The previous wave's members closed after failing; this wave is new.
        // One pass settles the previous attempt and admits the wave.
        let keys = (0..WAVE)
            .map(|member| format!("m{wave:03}-{member:02}"))
            .collect::<BTreeSet<_>>();
        *host.candidates.borrow_mut() = keys.clone();
        *host.withheld.borrow_mut() = (0..WITHHELD)
            .map(|task| format!("w{wave:03}-{task:02}"))
            .collect();
        assert_eq!(
            failing_tick(store.as_ref(), &host, i64::try_from(wave).unwrap()),
            "fired",
            "wave {wave}: new members are still admitted after {} distinct failures",
            failed_keys.len()
        );
        failed_keys.extend(keys);
    }

    // Settle the last wave while its members are still live: each failure is
    // recorded, and none refires at the fingerprint it failed at.
    let minute = i64::try_from(WAVES).unwrap();
    for minute in [minute, minute + 10] {
        assert_eq!(
            failing_tick(store.as_ref(), &host, minute),
            "needs_attention"
        );
    }
    let members = store
        .automation_state("fixture/member-failures")
        .unwrap()
        .unwrap()
        .members
        .unwrap();
    assert!(members.active.is_none());
    assert!(
        host.candidates
            .borrow()
            .iter()
            .all(|key| members.failed[key].exhausted)
    );
    assert!(failed_keys.len() > 1000);
    assert!(members.failed.len() <= 1000);
}
