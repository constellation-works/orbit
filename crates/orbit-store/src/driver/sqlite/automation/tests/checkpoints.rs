use crate::{Store, compose};
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
