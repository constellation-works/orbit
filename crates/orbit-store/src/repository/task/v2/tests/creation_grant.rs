//! A task's context creation grant commits or aborts with the scope write
//! that records it, is re-bound by every scope write that maintains it, and
//! cannot be revived or forged by a write that does not.

use orbit_types::task::{
    CONTEXT_CREATION_AUTHORIZED_EVENT, ContextCreationState, TaskComplexity, TaskHistoryEntry,
    TaskRelation, TaskRelationType,
};

use super::*;
use crate::contracts::{
    AtomicTaskMutationOutcome, AtomicTaskMutationParams, DesktopTaskMutationParams,
    TaskDocumentUpdateParams,
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

fn old_client_rescope(store: &TaskV2Store, id: &str, context_files: &[&str]) {
    store
        .with_task_lock(id, || {
            let mut bundle = store.bundle_store.read_bundle(id)?;
            bundle.envelope.context_files = scope(context_files);
            bundle.envelope.updated_at += chrono::Duration::seconds(1);
            store.bundle_store.rewrite_envelope(id, &bundle.envelope)
        })
        .expect("old client envelope update");
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
                expected_crew: bundle.envelope.crew.clone(),
                expected_crew_source: bundle.envelope.crew_source.clone(),
                crew: bundle.envelope.crew.clone(),
                crew_source: bundle.envelope.crew_source.clone(),
                expected_context_creation: expected,
                context_files: scope(context_files),
                status: bundle.envelope.status,
                complexity: TaskComplexity::Low,
                event_type: "task_pilot_applied".to_string(),
                event_note: "pilot".to_string(),
                history_summary: "pilot applied".to_string(),
                audit_note: "evidence".to_string(),
                append_history: vec![],
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

    // Two older-client writes bypass grant maintenance and restore the exact
    // authorized scope. The envelope revision binding prevents the first
    // grant from becoming current again after the second write.
    rescope(&store, &id, &[EXISTING, NEW], &[NEW]).expect("re-declare");
    old_client_rescope(&store, &id, &[EXISTING, OTHER_NEW]);
    assert_eq!(state(&store, &id), ContextCreationState::Void);
    old_client_rescope(&store, &id, &[EXISTING, NEW]);
    assert_eq!(state(&store, &id), ContextCreationState::Void);

    // The next maintained write records an explicit revocation, so a later
    // return to the voided grant's scope finds nothing to revive.
    rescope(&store, &id, &[EXISTING, OTHER_NEW], &[]).expect("maintained write");
    rescope(&store, &id, &[EXISTING, NEW], &[]).expect("scope of the voided grant");
    assert_eq!(state(&store, &id).selectors(), [] as [String; 0]);
}

#[test]
fn maintained_document_edits_rebind_creation_intent_to_the_new_revision() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let id = granted_task(&store, &[EXISTING, NEW], &[NEW]);
    let ContextCreationState::Current(before_grant) = state(&store, &id) else {
        panic!("authorized task has a current grant");
    };
    let before = before_grant.identity();

    store
        .update_task_document(
            &id,
            &TaskDocumentUpdateParams {
                actor: "operator".to_string(),
                title: Some("renamed task".to_string()),
                ..Default::default()
            },
        )
        .expect("maintained document update");

    let after = state(&store, &id);
    assert_eq!(after.selectors(), [NEW]);
    let ContextCreationState::Current(after_grant) = after else {
        panic!("maintained edit keeps the grant current");
    };
    assert_eq!(after_grant.identity(), before);
    assert_eq!(after_grant.generation, before_grant.generation);
    assert_ne!(after_grant.updated_at, before_grant.updated_at);
}

#[test]
fn desktop_comment_refreshes_the_revision_seal_without_changing_intent() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let id = granted_task(&store, &[EXISTING, NEW], &[NEW]);
    let ContextCreationState::Current(before_grant) = state(&store, &id) else {
        panic!("authorized task has a current grant");
    };
    let revision = store.desktop_task_revision(&id).expect("desktop revision");

    assert_eq!(
        store
            .apply_desktop_task_mutation(
                &id,
                &DesktopTaskMutationParams {
                    actor: "operator".to_string(),
                    request_id: "review-comment".to_string(),
                    payload_digest: "a".repeat(64),
                    expected_revision: revision,
                    fields: Default::default(),
                    crew_source: None,
                    comment: Some("A benign note.".to_string()),
                    status: None,
                },
            )
            .expect("desktop comment"),
        AtomicTaskMutationOutcome::Applied
    );

    let ContextCreationState::Current(after_grant) = state(&store, &id) else {
        panic!("desktop comment keeps the grant current");
    };
    assert_eq!(after_grant.identity(), before_grant.identity());
    assert_eq!(after_grant.generation, before_grant.generation);
    assert_ne!(after_grant.updated_at, before_grant.updated_at);
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

/// Reauthorizing the exact same scope creates a new preparation generation:
/// an assessment made before revoke + reauthorize must not pass the store CAS.
#[test]
fn revoke_and_same_scope_reauthorization_changes_the_pilot_generation() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let id = granted_task(&store, &[EXISTING, NEW], &[NEW]);
    let prepared_identity = state(&store, &id).identity();
    assert!(prepared_identity.is_some());

    rescope(&store, &id, &[EXISTING], &[]).expect("remove authorized selector");
    rescope(&store, &id, &[EXISTING, NEW], &[NEW]).expect("explicitly reauthorize same scope");
    let current_identity = state(&store, &id).identity();
    assert_ne!(current_identity, prepared_identity);
    assert_eq!(state(&store, &id).selectors(), [NEW]);

    assert_eq!(
        pilot_write(&store, &id, prepared_identity, &[EXISTING, NEW]),
        AtomicTaskMutationOutcome::Stale,
        "an old preparation cannot apply against a newly recorded grant with identical contents"
    );
}

/// Only scope writes record a grant; a caller-supplied history row cannot.
#[test]
fn a_history_append_cannot_forge_a_grant() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let id = granted_task(&store, &[EXISTING, NEW], &[]);
    let task = store.get_task(&id).expect("get task").expect("task exists");
    let forged = orbit_types::task::ContextCreationGrant::new(
        &id,
        scope(&[NEW]),
        &scope(&[EXISTING, NEW]),
        task.updated_at,
    );
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
