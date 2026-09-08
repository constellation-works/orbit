use crate::{Store, compose};
use chrono::{TimeZone, Utc};
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
