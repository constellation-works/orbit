use crate::contracts::AutomationStoreBackend;
use crate::{Store, compose};
use chrono::{TimeZone, Utc};
use orbit_types::workflow::automation::recovery::{RecoveryRecord, ReissuedAction};
use orbit_types::workflow::automation::*;

fn state() -> AutomationState {
    let head = SourceRevision {
        commit: "a".into(),
        tree: "tree-a".into(),
    };
    AutomationState {
        members: None,
        consumer: "owner/ws/qa".into(),
        epoch: "epoch".into(),
        trigger: None,
        repository: "repo".into(),
        branch: "agent-main".into(),
        generation: 0,
        baseline: head.clone(),
        observed: head.clone(),
        covered: head,
        pending_commits: vec![],
        pending: vec![],
        waived: vec![],
        excluded: vec![],
        unresolved: Default::default(),
        associations: Default::default(),
        active: None,
    }
}

#[test]
fn compare_exchange_survives_reopen_and_fences_stale_writers() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("orbit.db");
    let a = compose::automation_store(Store::open(&db).unwrap()).unwrap();
    let b = compose::automation_store(Store::open(&db).unwrap()).unwrap();
    let old = state();
    assert!(a.automation_initialize(&old).unwrap());
    assert!(!b.automation_initialize(&old).unwrap());
    let mut next = old.clone();
    next.generation = 1;
    next.observed.commit = "b".into();
    next.pending_commits.push("b".into());
    assert!(a.automation_commit(&old, &next, None).unwrap());
    assert!(!b.automation_commit(&old, &next, None).unwrap());
    drop(a);
    drop(b);
    let reopened = compose::automation_store(Store::open(&db).unwrap()).unwrap();
    assert_eq!(
        reopened.automation_state(&old.consumer).unwrap(),
        Some(next)
    );
}

#[test]
fn observation_cannot_advance_coverage() {
    let store = compose::automation_store(Store::open_in_memory().unwrap()).unwrap();
    let old = state();
    store.automation_initialize(&old).unwrap();
    let mut next = old.clone();
    next.generation = 1;
    next.covered.commit = "forged".into();
    assert!(store.automation_commit(&old, &next, None).is_err());
    assert_eq!(store.automation_state(&old.consumer).unwrap(), Some(old));
}

fn excluded_landing(commit: &str, tree: &str) -> ExcludedDelivery {
    let revision = SourceRevision {
        commit: commit.into(),
        tree: tree.into(),
    };
    ExcludedDelivery {
        delivery: Delivery {
            key: format!("pr:{commit}"),
            repository: "repo".into(),
            branch: "agent-main".into(),
            before: SourceRevision {
                commit: "a".into(),
                tree: "tree-a".into(),
            },
            after: revision.clone(),
            commits: vec![commit.into()],
            task_ids: vec!["task".into()],
            evidence_reference: "https://example.test/pr".into(),
            evidence_digest: "digest".into(),
            landed_at: Utc.with_ymd_and_hms(2026, 9, 6, 0, 0, 0).unwrap(),
        },
        exclusion: DeliveryExclusion {
            attempt_id: "rvw-1".into(),
            assurance: "independent_review".into(),
            task_meaning_digest: "meaning".into(),
            final_candidate_tree: tree.into(),
        },
        decided_at: Utc.with_ymd_and_hms(2026, 9, 6, 0, 0, 0).unwrap(),
    }
}

fn observed_excluded(previous: &AutomationState) -> AutomationState {
    let mut next = previous.clone();
    next.generation = previous.generation + 1;
    next.observed = SourceRevision {
        commit: "b".into(),
        tree: "tree-b".into(),
    };
    next.pending_commits.push("b".into());
    next.excluded.push(excluded_landing("b", "tree-b"));
    next
}

#[test]
fn excluded_prefix_may_advance_coverage_without_a_receipt() {
    let store = compose::automation_store(Store::open_in_memory().unwrap()).unwrap();
    let old = state();
    store.automation_initialize(&old).unwrap();
    let observed = observed_excluded(&old);
    assert!(store.automation_commit(&old, &observed, None).unwrap());

    let mut retired = observed.clone();
    retired.generation = observed.generation + 1;
    retired.covered = observed.observed.clone();
    retired.pending_commits.clear();
    retired.excluded.clear();
    assert!(store.automation_commit(&observed, &retired, None).unwrap());
    assert_eq!(
        store.automation_state(&old.consumer).unwrap(),
        Some(retired)
    );
    assert!(
        store
            .automation_receipts(&old.consumer, 20)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn excluded_prefix_cannot_drop_pending_debt() {
    let store = compose::automation_store(Store::open_in_memory().unwrap()).unwrap();
    let old = state();
    store.automation_initialize(&old).unwrap();
    let mut observed = observed_excluded(&old);
    observed.pending.push(observed.excluded[0].delivery.clone());
    assert!(store.automation_commit(&old, &observed, None).unwrap());

    let mut retired = observed.clone();
    retired.generation = observed.generation + 1;
    retired.covered = observed.observed.clone();
    retired.pending_commits.clear();
    retired.excluded.clear();
    assert!(store.automation_commit(&observed, &retired, None).is_err());
    assert_eq!(
        store.automation_state(&old.consumer).unwrap(),
        Some(observed)
    );
}

fn at() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 8, 0, 0, 0).unwrap()
}

fn landing() -> Delivery {
    Delivery {
        key: "pr:repo:1".into(),
        repository: "repo".into(),
        branch: "agent-main".into(),
        before: SourceRevision {
            commit: "a".into(),
            tree: "tree-a".into(),
        },
        after: SourceRevision {
            commit: "b".into(),
            tree: "tree-b".into(),
        },
        commits: vec!["b".into()],
        task_ids: vec!["ORB-1".into()],
        evidence_reference: "https://example.test/pr/1".into(),
        evidence_digest: "digest".into(),
        landed_at: at(),
    }
}

/// A consumer holding one unpaid landing whose action settled without evidence,
/// exactly the shape an archived unevidenced task leaves behind.
fn settled(store: &dyn AutomationStoreBackend) -> AutomationState {
    let baseline = state();
    assert!(store.automation_initialize(&baseline).unwrap());

    let landing = landing();
    let mut observed = baseline.clone();
    observed.generation = 1;
    observed.observed = landing.after.clone();
    observed.pending_commits = landing.commits.clone();
    observed.pending = vec![landing.clone()];
    assert!(store.automation_commit(&baseline, &observed, None).unwrap());

    let batch = CoverageBatch {
        schema_version: 1,
        id: "batch-1".into(),
        consumer: observed.consumer.clone(),
        epoch: observed.epoch.clone(),
        repository: observed.repository.clone(),
        branch: observed.branch.clone(),
        coverage: CoverageClass::IntegratedQaV1,
        from_exclusive: observed.covered.clone(),
        through_inclusive: landing.after.clone(),
        commits: landing.commits.clone(),
        deliveries: vec![landing],
        exclusions: vec![],
        created_at: at(),
        max_attempts: 1,
        retry_until: at(),
    };
    let mut claimed = observed.clone();
    claimed.generation = 2;
    claimed.active = Some(BatchAttempt {
        action_key: format!("automation:{}:1", batch.id),
        batch,
        input_digest: "frozen".into(),
        attempt: 1,
        action_id: None,
        state: BatchState::Claimed,
        reason: None,
        retry_after: None,
        reissue: None,
    });
    assert!(store.automation_commit(&observed, &claimed, None).unwrap());

    let mut settled = claimed.clone();
    settled.generation = 3;
    if let Some(active) = &mut settled.active {
        active.action_id = Some("ORB-11743".into());
        active.state = BatchState::Failed;
        active.reason = Some("task_closed_without_accepted_evidence".into());
    }
    assert!(store.automation_commit(&claimed, &settled, None).unwrap());

    settled
}

fn retuned(state: &AutomationState) -> DeliveryTrigger {
    DeliveryTrigger {
        owner_machine: Some("machine".into()),
        branch: state.branch.clone(),
        threshold: 3,
        max_wait_minutes: 60,
        coverage: CoverageClass::IntegratedQaV1,
        max_items: 20,
        retries: 0,
    }
}

fn adoption(previous: &AutomationState, next: &AutomationState) -> RecoveryRecord {
    RecoveryRecord {
        consumer: previous.consumer.clone(),
        previous_epoch: previous.epoch.clone(),
        epoch: next.epoch.clone(),
        previous_trigger: previous.trigger.clone(),
        trigger: next.trigger.clone(),
        adopted_settings: true,
        reissued: None,
        reason: "threshold retuned tonight".into(),
        by: "operator".into(),
        at: at(),
    }
}

/// The adopted identity moves and nothing else does.
fn adopted(previous: &AutomationState) -> AutomationState {
    let mut next = previous.clone();
    next.generation = previous.generation + 1;
    next.epoch = "retuned".into();
    next.trigger = Some(retuned(previous));
    next
}

#[test]
fn recovery_adopts_an_identity_and_records_it_without_touching_debt() {
    let store = compose::automation_store(Store::open_in_memory().unwrap()).unwrap();
    let previous = settled(store.as_ref());
    let next = adopted(&previous);

    assert!(
        store
            .automation_recover(&previous, &next, &adoption(&previous, &next))
            .unwrap()
    );

    let stored = store.automation_state(&previous.consumer).unwrap().unwrap();
    assert_eq!(stored.epoch, "retuned");
    assert_eq!(stored.covered, previous.covered);
    assert_eq!(stored.pending, previous.pending);
    assert_eq!(stored.active, previous.active);
    assert_eq!(
        store
            .automation_recoveries(&previous.consumer, 10)
            .unwrap()
            .len(),
        1
    );
    assert!(
        store
            .automation_receipts(&previous.consumer, 10)
            .unwrap()
            .is_empty(),
        "recovery never mints coverage"
    );
}

#[test]
fn recovery_cannot_skip_debt_or_advance_coverage() {
    let store = compose::automation_store(Store::open_in_memory().unwrap()).unwrap();
    let previous = settled(store.as_ref());

    let mut covered = adopted(&previous);
    covered.covered = previous.observed.clone();
    covered.pending.clear();
    covered.pending_commits.clear();

    let mut waived = adopted(&previous);
    waived.waived = waived.pending.drain(..).collect();

    let mut discarded = adopted(&previous);
    discarded.active = None;

    for forged in [covered, waived, discarded] {
        let record = adoption(&previous, &forged);
        assert!(
            store
                .automation_recover(&previous, &forged, &record)
                .is_err()
        );
        assert_eq!(
            store.automation_state(&previous.consumer).unwrap(),
            Some(previous.clone())
        );
    }
}

#[test]
fn recovery_requires_an_audit_record_of_the_exact_change() {
    let store = compose::automation_store(Store::open_in_memory().unwrap()).unwrap();
    let previous = settled(store.as_ref());
    let next = adopted(&previous);

    let unexplained = RecoveryRecord {
        reason: "  ".into(),
        ..adoption(&previous, &next)
    };
    let undeclared = RecoveryRecord {
        adopted_settings: false,
        ..adoption(&previous, &next)
    };
    let misattributed = RecoveryRecord {
        previous_epoch: "some other epoch".into(),
        ..adoption(&previous, &next)
    };

    for record in [unexplained, undeclared, misattributed] {
        assert!(store.automation_recover(&previous, &next, &record).is_err());
        assert_eq!(
            store.automation_state(&previous.consumer).unwrap(),
            Some(previous.clone())
        );
    }
}

#[test]
fn a_reissue_keeps_the_frozen_batch_and_names_the_action_it_replaces() {
    let store = compose::automation_store(Store::open_in_memory().unwrap()).unwrap();
    let previous = settled(store.as_ref());
    let settled_attempt = previous.active.clone().unwrap();

    let authorization = ActionReissue {
        from_action_id: settled_attempt.action_id.clone(),
        reason: "reissued for the unpaid QA obligations".into(),
        by: "operator".into(),
        at: at(),
        retry_until: at() + chrono::Duration::hours(24),
    };
    let mut next = previous.clone();
    next.generation = previous.generation + 1;
    if let Some(active) = &mut next.active {
        active.attempt = 2;
        active.action_key = format!("automation:{}:2", active.batch.id);
        active.action_id = None;
        active.reason = None;
        active.state = BatchState::Claimed;
        active.reissue = Some(authorization.clone());
    }
    let record = RecoveryRecord {
        consumer: previous.consumer.clone(),
        previous_epoch: previous.epoch.clone(),
        epoch: previous.epoch.clone(),
        previous_trigger: previous.trigger.clone(),
        trigger: previous.trigger.clone(),
        adopted_settings: false,
        reissued: Some(ReissuedAction {
            batch_id: settled_attempt.batch.id.clone(),
            from_action_id: settled_attempt.action_id.clone(),
            from_attempt: settled_attempt.attempt,
            from_state: settled_attempt.state,
            from_reason: settled_attempt.reason.clone(),
            attempt: 2,
            authorization: authorization.clone(),
        }),
        reason: authorization.reason.clone(),
        by: authorization.by.clone(),
        at: authorization.at,
    };

    // A reissue that quietly rewrites the frozen obligations is refused.
    let mut refrozen = next.clone();
    if let Some(active) = &mut refrozen.active {
        active.batch.deliveries.clear();
        active.batch.commits.clear();
    }
    assert!(
        store
            .automation_recover(&previous, &refrozen, &record)
            .is_err()
    );

    // So is one whose record does not name the action it replaced.
    let mislinked = RecoveryRecord {
        reissued: record.reissued.clone().map(|reissued| ReissuedAction {
            from_action_id: Some("ORB-99999".into()),
            ..reissued
        }),
        ..record.clone()
    };
    assert!(
        store
            .automation_recover(&previous, &next, &mislinked)
            .is_err()
    );

    assert!(store.automation_recover(&previous, &next, &record).unwrap());
    let stored = store.automation_state(&previous.consumer).unwrap().unwrap();
    assert_eq!(stored.active.as_ref().unwrap().batch, settled_attempt.batch);
    assert_eq!(stored.pending, previous.pending);
    assert_eq!(stored.covered, previous.covered);
}

#[test]
fn an_ordinary_checkpoint_cannot_move_the_identity_or_forge_an_authorization() {
    let store = compose::automation_store(Store::open_in_memory().unwrap()).unwrap();
    let previous = settled(store.as_ref());

    let mut adopted_without_audit = previous.clone();
    adopted_without_audit.generation = previous.generation + 1;
    adopted_without_audit.epoch = "retuned".into();

    let mut retriggered = previous.clone();
    retriggered.generation = previous.generation + 1;
    retriggered.trigger = Some(retuned(&previous));

    let mut self_authorized = previous.clone();
    self_authorized.generation = previous.generation + 1;
    if let Some(active) = &mut self_authorized.active {
        active.reissue = Some(ActionReissue {
            from_action_id: None,
            reason: "no operator asked for this".into(),
            by: "scheduler".into(),
            at: at(),
            retry_until: at() + chrono::Duration::hours(240),
        });
    }

    for forged in [adopted_without_audit, retriggered, self_authorized] {
        assert!(store.automation_commit(&previous, &forged, None).is_err());
        assert_eq!(
            store.automation_state(&previous.consumer).unwrap(),
            Some(previous.clone())
        );
    }
}
