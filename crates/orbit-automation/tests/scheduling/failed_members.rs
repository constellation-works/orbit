//! Failed member records stay bounded by what the source still observes, and
//! each records only its own member of the attempt it failed in.

use super::*;

/// Members each wave admits as one batch, all of which fail.
const WAVE: usize = 10;

/// Task ids each wave's page withholds beside them, filling the working set
/// the way a large backlog does, so departed members are retired regularly.
const WITHHELD: usize = 40;

/// Waves driven: their distinct failures pass the store's 1,000-record cap.
const WAVES: usize = 101;

const CONSUMER: &str = "fixture/member-failures";

/// A source whose members each fail their one attempt and then leave it,
/// the way a closed task leaves the statuses a task-pilot queries. Members
/// named in `applied` instead apply, so their attempt settles partially.
#[derive(Default)]
struct FailingHost {
    candidates: RefCell<BTreeSet<String>>,
    withheld: RefCell<BTreeSet<String>>,
    applied: RefCell<BTreeSet<String>>,
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
        let action_id = attempt.action_id.clone().unwrap_or_default();
        let applied = self.applied.borrow();
        let (certified, failed): (Vec<_>, Vec<_>) = attempt
            .members()
            .iter()
            .partition(|member| applied.contains(&member.key));
        Ok(MemberOutcome::Settled(MemberBatchEvidence {
            applied: certified
                .into_iter()
                .map(|member| MemberEvidence {
                    action_id: action_id.clone(),
                    attempt_id: attempt.id.clone(),
                    member_key: member.key.clone(),
                    input_fingerprint: member.fingerprint.clone(),
                    resulting_fingerprint: member.fingerprint.clone(),
                    ready: true,
                    result: serde_json::json!({"assessed": member.key}),
                })
                .collect(),
            failed: failed
                .into_iter()
                .map(|member| (member.key.clone(), "fixture_failure".into()))
                .collect(),
            action_id,
            attempt_id: attempt.id.clone(),
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
            consumer: CONSUMER,
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
        .automation_state(CONSUMER)
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
    assert!(
        members
            .failed
            .iter()
            .all(|(key, failed)| failed.members().len() == 1 && failed.member.key == *key),
        "failed state grows with the members it retains, not with their batches"
    );
}

fn state(store: &dyn AutomationStoreBackend) -> AutomationState {
    store.automation_state(CONSUMER).unwrap().unwrap()
}

/// The wave of `WAVE` members, all observed at one fingerprint.
fn wave(host: &FailingHost) -> Vec<String> {
    let keys = (0..WAVE)
        .map(|member| format!("m{member:02}"))
        .collect::<Vec<_>>();
    *host.candidates.borrow_mut() = keys.iter().cloned().collect();
    keys
}

#[test]
fn each_failed_member_keeps_its_own_record_and_forged_retirement_is_refused() {
    if isolated(
        "failed_members::each_failed_member_keeps_its_own_record_and_forged_retirement_is_refused",
    ) {
        return;
    }
    // A wholly failed attempt clears without a receipt; a partly applied one
    // certifies its applied members in a receipt and retires the rest.
    for applied in [0, 2] {
        let store = compose::automation_store(Store::open_in_memory().unwrap()).unwrap();
        let host = FailingHost::default();
        let keys = wave(&host);
        *host.applied.borrow_mut() = keys.iter().take(applied).cloned().collect();
        let (certified, failing) = keys.split_at(applied);

        assert_eq!(failing_tick(store.as_ref(), &host, 0), "fired");
        let previous = state(store.as_ref());
        let active = previous
            .members
            .as_ref()
            .and_then(|members| members.active.clone())
            .unwrap();
        assert_eq!(
            failing_tick(store.as_ref(), &host, 1),
            "needs_attention",
            "no failed member refires at the fingerprint it failed at"
        );
        let settled = state(store.as_ref());
        let receipt = store.automation_receipts(CONSUMER, 1).unwrap().pop();
        assert_eq!(receipt.is_some(), applied > 0);

        let members = settled.members.as_ref().unwrap();
        for key in failing {
            let failed = &members.failed[key];
            assert_eq!(Some(failed), active.failure_record(key).as_ref());
            assert_eq!(failed.members(), [active.member_for(key).unwrap().clone()]);
            assert!(
                serde_json::to_value(failed)
                    .unwrap()
                    .get("members")
                    .is_none(),
                "a failure record keeps the single-member wire shape earlier releases read as a batch of one"
            );
        }
        for key in certified {
            assert!(!members.failed.contains_key(key));
            assert_eq!(members.assessed[key].receipt_id, active.id);
        }

        // The settled checkpoint, replayed as one transition from the claim,
        // passes validation and only loses the generation fence.
        let mut genuine = settled.clone();
        genuine.generation = previous.generation + 1;
        assert!(
            !store
                .automation_commit(&previous, &genuine, receipt.as_ref())
                .unwrap()
        );

        let (first, second) = (&failing[0], &failing[1]);
        let forge = |name: &str, forge: &dyn Fn(&mut BTreeMap<String, MemberAttempt>)| {
            let mut forged = genuine.clone();
            forge(&mut forged.members.as_mut().unwrap().failed);
            assert!(
                store
                    .automation_commit(&previous, &forged, receipt.as_ref())
                    .is_err(),
                "applied={applied}: store accepted {name}"
            );
        };
        forge("a missing failure record", &|failed| {
            failed.remove(first);
        });
        forge("a record for a member outside the attempt", &|failed| {
            let mut outsider = failed[first].clone();
            outsider.member.key = "outsider".into();
            failed.insert("outsider".into(), outsider);
        });
        forge("a record of another attempt", &|failed| {
            failed.get_mut(first).unwrap().id = "other-attempt".into();
        });
        forge("a record at another attempt number", &|failed| {
            failed.get_mut(first).unwrap().attempt += 1;
        });
        forge("a record carrying a sibling member", &|failed| {
            let sibling = failed[second].clone();
            failed.insert(first.clone(), sibling);
        });
        forge("a record at a forged fingerprint", &|failed| {
            failed.get_mut(first).unwrap().member.fingerprint = "forged".into();
        });
        forge("a record left unexhausted", &|failed| {
            failed.get_mut(first).unwrap().exhausted = false;
        });
        forge("a whole-batch copy per member", &|failed| {
            let mut batch = active.clone();
            batch.exhausted = true;
            failed.insert(first.clone(), batch);
        });
        if let Some(certified) = certified.first() {
            forge("a failure record for a certified member", &|failed| {
                failed.insert(certified.clone(), active.failure_record(certified).unwrap());
            });
        }
    }
}

#[test]
fn checkpoint_with_whole_batch_failed_records_resumes_and_compacts() {
    if isolated("failed_members::checkpoint_with_whole_batch_failed_records_resumes_and_compacts") {
        return;
    }
    let base = Store::open_in_memory().unwrap();
    let store = compose::automation_store(base.clone()).unwrap();
    let host = FailingHost::default();
    let keys = wave(&host);
    assert_eq!(failing_tick(store.as_ref(), &host, 0), "fired");
    let active = state(store.as_ref())
        .members
        .and_then(|members| members.active)
        .unwrap();
    assert_eq!(failing_tick(store.as_ref(), &host, 1), "needs_attention");
    let compact = state(store.as_ref());

    // Persist the checkpoint a release before compact records wrote: every
    // failed member holding a copy of its whole exhausted batch, without the
    // defaulted excluded field added by a later release.
    let mut legacy = compact.clone();
    let mut batch = active.clone();
    batch.exhausted = true;
    for failed in legacy.members.as_mut().unwrap().failed.values_mut() {
        *failed = batch.clone();
    }
    let mut legacy_json = serde_json::to_value(&legacy).unwrap();
    legacy_json.as_object_mut().unwrap().remove("excluded");
    base.connection()
        .lock()
        .unwrap()
        .execute(
            "UPDATE automation_consumers SET state_json=?1 WHERE consumer=?2",
            [
                serde_json::to_string_pretty(&legacy_json).unwrap(),
                CONSUMER.to_string(),
            ],
        )
        .unwrap();
    assert_eq!(state(store.as_ref()), legacy);

    // Compaction may keep only the record's own member.
    let mut misattributed = legacy.clone();
    misattributed.generation += 1;
    let failed = &mut misattributed.members.as_mut().unwrap().failed;
    let other = failed[&keys[0]].failure_record(&keys[1]).unwrap();
    failed.insert(keys[0].clone(), other);
    assert!(
        store
            .automation_commit(&legacy, &misattributed, None)
            .is_err()
    );

    // The next pass reads it, still withholds every failed member and
    // commits each record compacted to its own member.
    assert_eq!(failing_tick(store.as_ref(), &host, 2), "needs_attention");
    let resumed = state(store.as_ref());
    assert_eq!(
        resumed.members.as_ref().unwrap().failed,
        compact.members.unwrap().failed
    );
    assert_eq!(resumed.generation, legacy.generation + 1);
}
