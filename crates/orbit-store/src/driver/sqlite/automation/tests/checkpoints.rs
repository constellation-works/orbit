use crate::contracts::AutomationStoreBackend;
use crate::{Store, compose};
use chrono::{TimeZone, Utc};
use orbit_types::workflow::automation::recovery::{
    HistoryMapping, HistoryReplayRecord, RecoveryRecord, ReissuedAction,
};
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
        stall: None,
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
        replayed_history: None,
        reset: None,
        friction_id: None,
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

fn replay_fixture() -> (AutomationState, AutomationState, RecoveryRecord) {
    let mut previous = state();
    previous.generation = 1;
    previous.observed = SourceRevision {
        commit: "orphan".into(),
        tree: "shared-tree".into(),
    };
    previous.pending_commits = vec!["orphan".into()];
    previous
        .unresolved
        .insert("orphan".into(), "evidence_pending".into());

    let canonical = SourceRevision {
        commit: "canonical".into(),
        tree: "shared-tree".into(),
    };
    let mut next = previous.clone();
    next.generation = 2;
    next.observed = canonical.clone();
    next.pending_commits = vec![canonical.commit.clone()];
    next.unresolved.clear();
    next.unresolved
        .insert(canonical.commit.clone(), "evidence_pending".into());

    let replay = HistoryReplayRecord {
        captured_generation: previous.generation,
        captured_head: SourceRevision {
            commit: "configured-head".into(),
            tree: "head-tree".into(),
        },
        common_base: previous.covered.clone(),
        old_observed: previous.observed.clone(),
        new_observed: canonical.clone(),
        mappings: vec![HistoryMapping {
            orphan: previous.observed.clone(),
            canonical,
            proof_digest: "exact-proof".into(),
        }],
        added_obligations: vec![],
        unchanged_baseline: previous.baseline.clone(),
        unchanged_covered: previous.covered.clone(),
        accepted_receipts: 0,
    };
    let record = RecoveryRecord {
        consumer: previous.consumer.clone(),
        previous_epoch: previous.epoch.clone(),
        epoch: previous.epoch.clone(),
        previous_trigger: previous.trigger.clone(),
        trigger: previous.trigger.clone(),
        adopted_settings: false,
        reissued: None,
        reset: None,
        friction_id: None,
        replayed_history: Some(replay),
        reason: "reconcile proven history".into(),
        by: "operator".into(),
        at: at(),
    };

    (previous, next, record)
}

#[test]
fn history_replay_migrates_the_exact_unresolved_reason_and_fences_generation() {
    let store = compose::automation_store(Store::open_in_memory().unwrap()).unwrap();
    let baseline = state();
    let (previous, next, record) = replay_fixture();
    assert!(store.automation_initialize(&baseline).unwrap());
    assert!(store.automation_commit(&baseline, &previous, None).unwrap());

    let mut changed_reason = next.clone();
    changed_reason
        .unresolved
        .insert("canonical".into(), "different_reason".into());
    assert!(
        store
            .automation_recover(&previous, &changed_reason, &record)
            .is_err()
    );
    assert_eq!(
        store.automation_state(&previous.consumer).unwrap(),
        Some(previous.clone())
    );

    assert!(store.automation_recover(&previous, &next, &record).unwrap());
    let audit = store.automation_recoveries(&previous.consumer, 10).unwrap();
    let replay = audit[0].replayed_history.as_ref().unwrap();
    assert_eq!(replay.old_observed, previous.observed);
    assert_eq!(replay.new_observed, next.observed);
    assert_eq!(replay.unchanged_covered, previous.covered);
    assert_eq!(replay.mappings[0].proof_digest, "exact-proof");

    let raced = compose::automation_store(Store::open_in_memory().unwrap()).unwrap();
    assert!(raced.automation_initialize(&baseline).unwrap());
    assert!(raced.automation_commit(&baseline, &previous, None).unwrap());
    let mut concurrent = previous.clone();
    concurrent.generation += 1;
    assert!(
        raced
            .automation_commit(&previous, &concurrent, None)
            .unwrap()
    );
    assert!(!raced.automation_recover(&previous, &next, &record).unwrap());
    assert!(
        raced
            .automation_recoveries(&previous.consumer, 10)
            .unwrap()
            .is_empty()
    );
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
    waived.waived = std::mem::take(&mut waived.pending);

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
        replayed_history: None,
        reset: None,
        friction_id: None,
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

/// The audit record is the only trace a forgotten consumer leaves, so the store
/// refuses one that does not describe the state it is destroying.
#[test]
fn a_reset_record_must_match_the_state_it_forgets_and_fences_on_it() {
    let store = compose::automation_store(Store::open_in_memory().unwrap()).unwrap();
    let mut previous = state();
    previous.pending_commits = vec!["c1".into()];
    previous
        .unresolved
        .insert("c1".into(), "evidence_pending".into());
    assert!(store.automation_initialize(&state()).unwrap());
    assert!(
        store
            .automation_commit(
                &state(),
                &{
                    let mut next = previous.clone();
                    next.generation = 1;
                    next
                },
                None
            )
            .unwrap()
    );
    previous.generation = 1;

    let record = reset_record(&previous);

    let mut mismatched = record.clone();
    if let Some(reset) = &mut mismatched.reset {
        reset.forgotten.pending_commits = 0;
    }
    assert!(
        store.automation_reset(&previous, &mismatched).is_err(),
        "an inventory that disagrees with the state is not an audit"
    );

    let mut unauthorized = record.clone();
    unauthorized.reason = "  ".into();
    assert!(store.automation_reset(&previous, &unauthorized).is_err());

    let mut stale = previous.clone();
    stale.generation = 0;
    assert!(
        !store
            .automation_reset(&stale, &reset_record(&stale))
            .unwrap(),
        "a consumer another pass already moved is not reset from stale facts"
    );

    assert!(store.automation_reset(&previous, &record).unwrap());
    assert_eq!(store.automation_state(&previous.consumer).unwrap(), None);
    let history = store.automation_recoveries(&previous.consumer, 10).unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].kind(), "reset");
    assert_eq!(
        history[0].reset.as_ref().unwrap().forgotten.pending_commits,
        1
    );
}

fn reset_record(previous: &AutomationState) -> RecoveryRecord {
    RecoveryRecord {
        consumer: previous.consumer.clone(),
        previous_epoch: previous.epoch.clone(),
        epoch: previous.epoch.clone(),
        previous_trigger: previous.trigger.clone(),
        trigger: previous.trigger.clone(),
        adopted_settings: false,
        reissued: None,
        replayed_history: None,
        reset: Some(orbit_types::workflow::automation::recovery::ResetRecord {
            previous_generation: previous.generation,
            forgotten: orbit_types::workflow::automation::recovery::CoverageDebt {
                baseline: previous.baseline.clone(),
                covered: previous.covered.clone(),
                observed: previous.observed.clone(),
                pending_deliveries: previous.pending.len(),
                pending_commits: previous.pending_commits.len(),
                unresolved: previous.unresolved.len(),
                waived: previous.waived.len(),
                excluded: previous.excluded.len(),
                receipts: 0,
            },
            abandoned_action: None,
            baseline: SourceRevision {
                commit: "new-head".into(),
                tree: "new-tree".into(),
            },
            released_refs: vec!["refs/orbit/automation/digest/batch/from".into()],
            cleared_stall: previous.stall.clone(),
        }),
        friction_id: None,
        reason: "the branch was rewritten past the observed commit".into(),
        by: "operator".into(),
        at: at(),
    }
}

/// A stall records why evaluation stopped. It may not move the consumer.
#[test]
fn a_stall_marker_moves_only_itself_under_the_generation_fence() {
    let store = compose::automation_store(Store::open_in_memory().unwrap()).unwrap();
    let previous = state();
    assert!(store.automation_initialize(&previous).unwrap());

    let stall = orbit_types::workflow::automation::recovery::AutomationStall {
        reason: "history_diverged".into(),
        since: at(),
        escalated_at: None,
        friction_id: None,
        divergence: None,
    };

    let mut unchanged = previous.clone();
    unchanged.generation = 1;
    assert!(
        store.automation_stall(&previous, &unchanged).is_err(),
        "a write that records no marker change is not a stall"
    );

    let mut moved = previous.clone();
    moved.generation = 1;
    moved.stall = Some(stall.clone());
    moved.observed = SourceRevision {
        commit: "moved".into(),
        tree: "moved-tree".into(),
    };
    assert!(
        store.automation_stall(&previous, &moved).is_err(),
        "a stall may not advance the observed cursor"
    );

    let mut marked = previous.clone();
    marked.generation = 1;
    marked.stall = Some(stall);
    assert!(store.automation_stall(&previous, &marked).unwrap());
    assert_eq!(
        store.automation_state(&previous.consumer).unwrap(),
        Some(marked.clone())
    );
    assert!(
        !store.automation_stall(&previous, &marked).unwrap(),
        "the fence rejects a second write from the stale generation"
    );

    let mut cleared = marked.clone();
    cleared.generation = 2;
    cleared.stall = None;
    assert!(store.automation_stall(&marked, &cleared).unwrap());
    assert_eq!(
        store
            .automation_state(&previous.consumer)
            .unwrap()
            .and_then(|state| state.stall),
        None
    );
}
