//! A task's context creation grant commits or aborts with the scope write
//! that records it, is re-bound by every scope write that maintains it, and
//! cannot be revived or forged by a write that does not.

use orbit_types::task::{
    CONTEXT_CREATION_AUTHORIZED_EVENT, ContextCreationState, TaskComplexity, TaskHistoryEntry,
    TaskRelation, TaskRelationType,
};

use super::*;
use crate::contracts::{
    AtomicTaskMutationOutcome, AtomicTaskMutationParams, TaskDocumentUpdateParams,
};

const EXISTING: &str = "file:README.md";
const NEW: &str = "file:src/new.rs";
const OTHER_NEW: &str = "file:src/other.rs";

fn scope(selectors: &[&str]) -> Vec<String> {
    selectors
        .iter()
        .map(|selector| selector.to_string())
        .collect()
}

fn granted_task(store: &TaskV2Store, context_files: &[&str], authorize: &[&str]) -> String {
    let mut params = create_params("Grant", TaskStatus::Proposed);
    params.context_files = scope(context_files);
    params.context_creation = scope(authorize);
    store.create_task(params).expect("create task").id
}

fn state(store: &TaskV2Store, id: &str) -> ContextCreationState {
    creation_state(&store.bundle_store.read_bundle(id).expect("read bundle"))
}

fn rescope(
    store: &TaskV2Store,
    id: &str,
    context_files: &[&str],
    authorize: &[&str],
) -> Result<(), OrbitError> {
    store.update_task_document(
        id,
        &TaskDocumentUpdateParams {
            actor: "operator".to_string(),
            context_files: Some(scope(context_files)),
            context_creation: scope(authorize),
            ..Default::default()
        },
    )
}

fn pilot_write(
    store: &TaskV2Store,
    id: &str,
    expected: Option<String>,
    context_files: &[&str],
) -> AtomicTaskMutationOutcome {
    let bundle = store.bundle_store.read_bundle(id).expect("read bundle");
    store
        .apply_atomic_task_mutation(
            id,
            &AtomicTaskMutationParams {
                actor: "pilot".to_string(),
                operation_id: format!("op-{}", context_files.len()),
                expected_context_files: bundle.envelope.context_files.clone(),
                expected_status: bundle.envelope.status,
                expected_complexity: bundle.envelope.complexity,
                expected_context_creation: expected,
                context_files: scope(context_files),
                status: bundle.envelope.status,
                complexity: TaskComplexity::Low,
                event_type: "task_pilot_applied".to_string(),
                event_note: "pilot".to_string(),
                history_summary: "pilot applied".to_string(),
                audit_note: "evidence".to_string(),
            },
        )
        .expect("atomic mutation")
}

/// Fault injection: the write fails after the grant row was appended, on a
/// relation it cannot store, and the pending-write journal takes the row
/// back with the scope.
#[test]
fn a_failed_scope_write_leaves_neither_its_scope_nor_its_grant() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let id = granted_task(&store, &[EXISTING, NEW], &[NEW]);
    let before = store.bundle_store.read_bundle(&id).expect("read bundle");

    let failed = store.update_task_document(
        &id,
        &TaskDocumentUpdateParams {
            actor: "operator".to_string(),
            context_files: Some(scope(&[EXISTING, NEW, OTHER_NEW])),
            context_creation: scope(&[OTHER_NEW]),
            relations: Some(vec![TaskRelation {
                relation_type: TaskRelationType::RelatedTo,
                target: "ORB-99999".to_string(),
            }]),
            ..Default::default()
        },
    );
    assert!(
        failed.is_err(),
        "the fixture write must fail after the grant append"
    );
    let after = store.bundle_store.read_bundle(&id).expect("read bundle");
    assert_eq!(after.events, before.events);
    assert_eq!(after.envelope.context_files, before.envelope.context_files);
    assert_eq!(state(&store, &id).selectors(), [NEW]);

    rescope(&store, &id, &[EXISTING, NEW, OTHER_NEW], &[OTHER_NEW]).expect("retry");
    assert_eq!(state(&store, &id).selectors(), [NEW, OTHER_NEW]);
}

/// Removal revokes, and once revoked nothing short of a new declaration
/// grants the selector again — not a restored scope, and not a write that
/// returns the scope a voided grant was bound to.
#[test]
fn revoked_or_voided_grants_never_revive() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let id = granted_task(&store, &[EXISTING, NEW], &[NEW]);

    rescope(&store, &id, &[EXISTING], &[]).expect("revoke by removal");
    assert!(
        matches!(state(&store, &id), ContextCreationState::Current(ref grant) if grant.selectors.is_empty())
    );
    rescope(&store, &id, &[EXISTING, NEW], &[]).expect("restore scope");
    assert_eq!(state(&store, &id).selectors(), [] as [String; 0]);

    // A writer that does not maintain grants (an older client) changes the
    // scope: the grant it bypassed is void, not carried.
    rescope(&store, &id, &[EXISTING, NEW], &[NEW]).expect("re-declare");
    store
        .with_task_lock(&id, || {
            let mut bundle = store.bundle_store.read_bundle(&id)?;
            bundle.envelope.context_files = scope(&[EXISTING, NEW, OTHER_NEW]);
            store.bundle_store.rewrite_envelope(&id, &bundle.envelope)
        })
        .expect("unmaintained rewrite");
    assert_eq!(state(&store, &id), ContextCreationState::Void);

    // The next maintained write records an explicit revocation, so a later
    // return to the voided grant's own scope finds nothing to revive.
    rescope(&store, &id, &[EXISTING, OTHER_NEW], &[]).expect("maintained write");
    rescope(&store, &id, &[EXISTING, NEW], &[]).expect("scope of the voided grant");
    assert_eq!(state(&store, &id).selectors(), [] as [String; 0]);
}

/// The pilot's write carries the grant it validated against: a different
/// grant at the write boundary is stale and writes nothing, the same one is
/// retained and re-bound to the pilot's scope.
#[test]
fn the_atomic_pilot_write_compares_and_carries_the_grant() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let id = granted_task(&store, &[EXISTING, NEW], &[NEW]);
    let identity = state(&store, &id).identity();
    assert!(identity.is_some());

    let events = store.bundle_store.read_bundle(&id).expect("read").events;
    assert_eq!(
        pilot_write(&store, &id, None, &[EXISTING, NEW]),
        AtomicTaskMutationOutcome::Stale
    );
    assert_eq!(
        store.bundle_store.read_bundle(&id).expect("read").events,
        events
    );

    assert_eq!(
        pilot_write(
            &store,
            &id,
            identity.clone(),
            &[NEW, EXISTING, "file:Cargo.toml"]
        ),
        AtomicTaskMutationOutcome::Applied
    );
    let carried = state(&store, &id);
    assert_eq!(carried.selectors(), [NEW]);
    assert_ne!(
        carried.identity(),
        identity,
        "re-bound to the pilot's scope"
    );
}

/// Only scope writes record a grant; a caller-supplied history row cannot.
#[test]
fn a_history_append_cannot_forge_a_grant() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let id = granted_task(&store, &[EXISTING, NEW], &[]);
    let forged =
        orbit_types::task::ContextCreationGrant::new(&id, scope(&[NEW]), &scope(&[EXISTING, NEW]));
    let refused = store.update_task_history(
        &id,
        &TaskHistoryUpdateParams {
            actor: "agent".to_string(),
            append_history: vec![TaskHistoryEntry {
                at: Utc::now(),
                by: "agent".to_string(),
                event: CONTEXT_CREATION_AUTHORIZED_EVENT.to_string(),
                note: Some(forged.to_note()),
                from_status: None,
                to_status: None,
            }],
            ..Default::default()
        },
    );
    assert!(refused.is_err());
    assert_eq!(state(&store, &id), ContextCreationState::Absent);
}

/// Interleaved maintained writers serialize on the task lock, so whichever
/// commits last leaves a grant bound to the scope it stored.
#[test]
fn concurrent_scope_writes_leave_a_grant_bound_to_the_final_scope() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let id = granted_task(&store, &[EXISTING, NEW], &[NEW]);
    std::thread::scope(|threads| {
        for writer in 0..4 {
            let (store, id) = (&store, &id);
            threads.spawn(move || {
                for round in 0..5 {
                    let result = if (writer + round) % 2 == 0 {
                        rescope(store, id, &[EXISTING, NEW, OTHER_NEW], &[OTHER_NEW])
                    } else {
                        rescope(store, id, &[EXISTING, NEW], &[])
                    };
                    result.expect("scope write");
                }
            });
        }
    });
    let bundle = store.bundle_store.read_bundle(&id).expect("read bundle");
    let ContextCreationState::Current(grant) = creation_state(&bundle) else {
        panic!("a maintained write never leaves the grant void");
    };
    assert!(grant.selectors.contains(&NEW.to_string()));
    let ids = bundle
        .events
        .iter()
        .map(|event| &event.event_id)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(ids.len(), bundle.events.len(), "event ids stay unique");
}
