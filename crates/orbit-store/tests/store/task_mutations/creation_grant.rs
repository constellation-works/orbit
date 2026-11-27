//! A task's context creation grant commits or aborts with the scope write
//! that records it, is re-bound by every scope write that maintains it, and
//! cannot be revived or forged by a write that does not.

use orbit_types::task::{
    CONTEXT_CREATION_AUTHORIZED_EVENT, ContextCreationState, TaskComplexity, TaskHistoryEntry,
    TaskRelation, TaskRelationType,
};

use super::*;
use orbit_store::contract::{
    AtomicTaskMutationOutcome, AtomicTaskMutationParams, DesktopTaskMutationParams,
    TaskDocumentUpdateParams,
};

use chrono::{DateTime, Utc};
use orbit_types::identity::OrbitId;
use orbit_types::task::{CrewSource, ExternalRef, TaskPriority, TaskType};
use serde::{Deserialize, Serialize};

const EXISTING: &str = "file:README.md";
const NEW: &str = "file:src/new.rs";
const OTHER_NEW: &str = "file:src/other.rs";

// Frozen from TaskEnvelopeV2 at 98e987b9eb^; unknown fields are rejected.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct PriorTaskEnvelopeV2 {
    pub schema_version: u32,
    pub id: OrbitId,
    pub title: String,
    pub status: TaskStatus,
    #[serde(rename = "type")]
    pub task_type: TaskType,
    pub priority: TaskPriority,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub complexity: Option<TaskComplexity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_run_id: Option<String>,
    #[serde(
        default,
        alias = "job_run_host",
        skip_serializing_if = "Option::is_none"
    )]
    pub job_run_machine: Option<orbit_types::task::ExecutionLocation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crew: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crew_source: Option<CrewSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orchestrator: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orchestrator_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orchestrator_model_source: Option<orbit_types::task::OrchestratorModelSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<orbit_types::task::TaskOrigin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule_fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relations: Vec<TaskRelation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub granted_tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires_labels: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_before: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub context_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub external_refs: Vec<ExternalRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub planned_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub implemented_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

fn scope(selectors: &[&str]) -> Vec<String> {
    selectors
        .iter()
        .map(|selector| selector.to_string())
        .collect()
}

fn granted_task(
    store: &WorkspaceTaskBackends,
    context_files: &[&str],
    authorize: &[&str],
) -> String {
    let mut params = create_params("Grant", TaskStatus::Proposed);
    params.context_files = scope(context_files);
    params.context_creation = scope(authorize);
    store.task.create_task(params).expect("create task").id
}

fn state(_store: &WorkspaceTaskBackends, id: &str) -> ContextCreationState {
    creation_state(&snapshot(id))
}

fn old_client_rescope(_store: &WorkspaceTaskBackends, id: &str, context_files: &[&str]) {
    let mut envelope = snapshot(id).envelope;
    envelope.context_files = scope(context_files);
    envelope.updated_at += chrono::Duration::seconds(1);
    std::fs::write(
        bundle_path(id).join(TASK_ENVELOPE_FILE_NAME),
        serde_yaml::to_string(&envelope).unwrap(),
    )
    .unwrap();
}

fn creation_state(bundle: &Snapshot) -> ContextCreationState {
    ContextCreationState::resolve(
        &bundle.envelope.id,
        &bundle.envelope.context_files,
        bundle.envelope.updated_at,
        bundle.envelope.context_creation_sealed_at,
        bundle
            .events
            .iter()
            .map(|event| (event.event_type.as_str(), event.note.as_deref())),
    )
}

fn grant_event_count(id: &str) -> usize {
    snapshot(id)
        .events
        .iter()
        .filter(|event| event.event_type == CONTEXT_CREATION_AUTHORIZED_EVENT)
        .count()
}

fn rescope(
    store: &WorkspaceTaskBackends,
    id: &str,
    context_files: &[&str],
    authorize: &[&str],
) -> Result<(), OrbitError> {
    store
        .document
        .update_task_document(
            id,
            TaskDocumentUpdateParams {
                actor: "operator".to_string(),
                context_files: Some(scope(context_files)),
                context_creation: scope(authorize),
                ..Default::default()
            },
        )
        .map(|_| ())
}

fn pilot_write(
    store: &WorkspaceTaskBackends,
    id: &str,
    expected: Option<String>,
    context_files: &[&str],
) -> AtomicTaskMutationOutcome {
    let bundle = snapshot(id);
    store
        .task
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
                add_tags: Vec::new(),
                add_dependencies: Vec::new(),
                status: bundle.envelope.status,
                complexity: TaskComplexity::Low,
                event_type: "task_pilot_applied".to_string(),
                event_note: "pilot".to_string(),
                history_summary: "pilot applied".to_string(),
                audit_note: "evidence".to_string(),
                append_history: vec![],
                decision: None,
            },
        )
        .expect("atomic mutation")
}

#[test]
fn prior_writer_rejects_sealed_envelopes_and_lost_seals_fail_closed() {
    if !support::isolated(
        "creation_grant::prior_writer_rejects_sealed_envelopes_and_lost_seals_fail_closed",
    ) {
        return;
    }
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let id = granted_task(&store, &[EXISTING, NEW], &[NEW]);
    let path = bundle_path(&id).join(TASK_ENVELOPE_FILE_NAME);

    // Persist a prior-format revision-bound grant through the frozen codec.
    let mut initial = snapshot(&id).envelope;
    initial.context_creation_sealed_at = None;
    let prior: PriorTaskEnvelopeV2 =
        serde_yaml::from_str(&serde_yaml::to_string(&initial).unwrap()).unwrap();
    std::fs::write(&path, serde_yaml::to_string(&prior).unwrap()).unwrap();
    let ContextCreationState::Current(grant) = state(&store, &id) else {
        panic!("prior revision-bound grant must be current");
    };
    assert_eq!(grant.selectors, scope(&[NEW]));
    store
        .document
        .update_task_document(
            &id,
            TaskDocumentUpdateParams {
                actor: "operator".to_string(),
                tags: Some(vec!["maintained".to_string()]),
                ..Default::default()
            },
        )
        .expect("maintained update");
    let sealed = snapshot(&id);
    assert_eq!(creation_state(&sealed).selectors(), &[NEW]);
    assert_ne!(grant.updated_at, Some(sealed.envelope.updated_at));
    let yaml = std::fs::read_to_string(&path).unwrap();
    let error = serde_yaml::from_str::<PriorTaskEnvelopeV2>(&yaml).unwrap_err();
    assert!(
        error.to_string().contains("context_creation_sealed_at"),
        "{error}"
    );

    // The old reader cannot itself drop the seal: deny_unknown_fields rejects
    // it. Model independent seal loss, then exercise the old codec and its
    // revision-bound resolver on that altered disposable bundle.
    let mut unsealed = sealed.envelope;
    unsealed.context_creation_sealed_at = None;
    std::fs::write(&path, serde_yaml::to_string(&unsealed).unwrap()).unwrap();
    assert_eq!(state(&store, &id), ContextCreationState::Void);
    let mut prior: PriorTaskEnvelopeV2 =
        serde_yaml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    prior.updated_at += chrono::Duration::seconds(1);
    std::fs::write(&path, serde_yaml::to_string(&prior).unwrap()).unwrap();
    assert_eq!(state(&store, &id), ContextCreationState::Void);
    rescope(&store, &id, &[EXISTING, NEW], &[]).expect("maintained write after seal loss");
    assert!(state(&store, &id).selectors().is_empty());
}

/// Fault injection: the write fails after the grant row was appended, on a
/// relation it cannot store, and the pending-write journal takes the row
/// back with the scope.
#[test]
fn a_failed_scope_write_leaves_neither_its_scope_nor_its_grant() {
    if !support::isolated(
        "creation_grant::a_failed_scope_write_leaves_neither_its_scope_nor_its_grant",
    ) {
        return;
    }
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let id = granted_task(&store, &[EXISTING, NEW], &[NEW]);
    let before = snapshot(&id);

    let failed = store.document.update_task_document(
        &id,
        TaskDocumentUpdateParams {
            actor: "operator".to_string(),
            context_files: Some(scope(&[EXISTING, NEW, OTHER_NEW])),
            context_creation: scope(&[OTHER_NEW]),
            relations: Some(vec![TaskRelation {
                relation_type: TaskRelationType::RelatedTo,
                target: "ORB-99999".to_string(),
                regression: None,
            }]),
            ..Default::default()
        },
    );
    assert!(
        failed.is_err(),
        "the fixture write must fail after the grant append"
    );
    let after = snapshot(&id);
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
    if !support::isolated("creation_grant::revoked_or_voided_grants_never_revive") {
        return;
    }
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

    // An unmaintained writer that advances only `updated_at` without updating
    // `context_creation_sealed_at` also voids the grant fail-closed.
    rescope(&store, &id, &[EXISTING, NEW], &[NEW]).expect("re-declare before timestamp drift");
    old_client_rescope(&store, &id, &[EXISTING, NEW]);
    assert_eq!(state(&store, &id), ContextCreationState::Void);

    // The next maintained write records an explicit revocation, so a later
    // return to the voided grant's scope finds nothing to revive.
    rescope(&store, &id, &[EXISTING, OTHER_NEW], &[]).expect("maintained write");
    rescope(&store, &id, &[EXISTING, NEW], &[]).expect("scope of the voided grant");
    assert_eq!(state(&store, &id).selectors(), [] as [String; 0]);

    // Legacy bundles whose `task.yaml` lacks `context_creation_sealed_at`
    // while `events.jsonl` holds historical `context_creation_authorized` rows
    // continue to resolve via `grant.updated_at`, carry forward on unrelated
    // writes without appending duplicates, and fail closed on unmaintained
    // envelope updates.
    rescope(&store, &id, &[EXISTING, NEW], &[NEW]).expect("grant for legacy bundle check");
    let legacy_count = grant_event_count(&id);
    let ContextCreationState::Current(legacy_grant) = state(&store, &id) else {
        panic!("active grant before stripping envelope seal");
    };
    let mut legacy_envelope = snapshot(&id).envelope;
    legacy_envelope.context_creation_sealed_at = None;
    std::fs::write(
        bundle_path(&id).join(TASK_ENVELOPE_FILE_NAME),
        serde_yaml::to_string(&legacy_envelope).unwrap(),
    )
    .unwrap();
    assert_eq!(
        state(&store, &id),
        ContextCreationState::Current(legacy_grant.clone()),
        "legacy bundle without context_creation_sealed_at resolves losslessly"
    );
    store
        .document
        .update_task_document(
            &id,
            TaskDocumentUpdateParams {
                actor: "operator".to_string(),
                tags: Some(vec!["legacy-check".to_string()]),
                ..Default::default()
            },
        )
        .expect("unrelated write on legacy bundle");
    assert_eq!(grant_event_count(&id), legacy_count);
    assert_eq!(
        state(&store, &id),
        ContextCreationState::Current(legacy_grant)
    );
    let migrated_envelope = snapshot(&id).envelope;
    assert_eq!(
        migrated_envelope.context_creation_sealed_at,
        Some(migrated_envelope.updated_at)
    );
}

#[test]
fn maintained_document_edits_rebind_creation_intent_to_the_new_revision() {
    if !support::isolated(
        "creation_grant::maintained_document_edits_rebind_creation_intent_to_the_new_revision",
    ) {
        return;
    }
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let id = granted_task(&store, &[EXISTING, NEW], &[NEW]);
    let initial_snapshot = snapshot(&id);
    let ContextCreationState::Current(before_grant) = creation_state(&initial_snapshot) else {
        panic!("authorized task has a current grant");
    };
    let before = before_grant.identity();
    assert_eq!(grant_event_count(&id), 1);
    assert_eq!(
        initial_snapshot.envelope.context_creation_sealed_at,
        Some(initial_snapshot.envelope.updated_at)
    );

    // Three unrelated writes (comment, status change, tag change) with
    // unchanged `context_files` append zero new `context_creation_authorized`
    // rows while keeping the grant current with identical selectors/generation.
    store
        .history
        .update_task_history(
            &id,
            TaskHistoryUpdateParams {
                actor: "operator".to_string(),
                append_comments: vec![orbit_types::task::TaskComment {
                    at: Utc::now(),
                    by: "operator".to_string(),
                    message: "Unrelated comment.".to_string(),
                    operation_id: None,
                }],
                ..Default::default()
            },
        )
        .expect("comment write");
    store
        .history
        .update_task_history(
            &id,
            TaskHistoryUpdateParams {
                actor: "operator".to_string(),
                status: Some(TaskStatus::Backlog),
                ..Default::default()
            },
        )
        .expect("status change");
    store
        .document
        .update_task_document(
            &id,
            TaskDocumentUpdateParams {
                actor: "operator".to_string(),
                title: Some("renamed task".to_string()),
                tags: Some(vec!["economy".to_string()]),
                ..Default::default()
            },
        )
        .expect("tag and title update");

    assert_eq!(
        grant_event_count(&id),
        1,
        "unrelated writes must append zero duplicate context_creation_authorized rows"
    );
    let after_snapshot = snapshot(&id);
    let after = creation_state(&after_snapshot);
    assert_eq!(after.selectors(), [NEW]);
    let ContextCreationState::Current(after_grant) = after else {
        panic!("maintained edits keep the grant current");
    };
    assert_eq!(after_grant.identity(), before);
    assert_eq!(after_grant.generation, before_grant.generation);
    assert_eq!(after_grant.updated_at, before_grant.updated_at);
    assert_ne!(
        after_snapshot.envelope.updated_at,
        initial_snapshot.envelope.updated_at
    );
    assert_eq!(
        after_snapshot.envelope.context_creation_sealed_at,
        Some(after_snapshot.envelope.updated_at)
    );

    // Mutating `context_files` or authorizing/revoking a selector still
    // appends exactly one `context_creation_authorized` row each time.
    rescope(&store, &id, &[EXISTING, NEW, OTHER_NEW], &[OTHER_NEW]).expect("authorize selector");
    assert_eq!(grant_event_count(&id), 2);
    assert_eq!(state(&store, &id).selectors(), [NEW, OTHER_NEW]);

    rescope(&store, &id, &[EXISTING, NEW], &[]).expect("revoke selector");
    assert_eq!(grant_event_count(&id), 3);
    assert_eq!(state(&store, &id).selectors(), [NEW]);
}

#[test]
fn desktop_comment_refreshes_the_revision_seal_without_changing_intent() {
    if !support::isolated(
        "creation_grant::desktop_comment_refreshes_the_revision_seal_without_changing_intent",
    ) {
        return;
    }
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let id = granted_task(&store, &[EXISTING, NEW], &[NEW]);
    let before_snapshot = snapshot(&id);
    let ContextCreationState::Current(before_grant) = creation_state(&before_snapshot) else {
        panic!("authorized task has a current grant");
    };
    let before_grant_events = grant_event_count(&id);
    let revision = store
        .task
        .desktop_task_revision(&id)
        .expect("desktop revision");

    assert_eq!(
        store
            .task
            .apply_desktop_task_mutation(
                &id,
                &DesktopTaskMutationParams {
                    write_provenance: None,
                    actor: "operator".to_string(),
                    request_id: "review-comment".to_string(),
                    payload_digest: "a".repeat(64),
                    expected_revision: revision,
                    fields: Default::default(),
                    crew_source: None,
                    comment: Some("A benign note.".to_string()),
                    status: None,
                    append_history: Vec::new(),
                },
            )
            .expect("desktop comment"),
        AtomicTaskMutationOutcome::Applied
    );

    assert_eq!(grant_event_count(&id), before_grant_events);
    let after_snapshot = snapshot(&id);
    let ContextCreationState::Current(after_grant) = creation_state(&after_snapshot) else {
        panic!("desktop comment keeps the grant current");
    };
    assert_eq!(after_grant.identity(), before_grant.identity());
    assert_eq!(after_grant.generation, before_grant.generation);
    assert_ne!(
        after_snapshot.envelope.updated_at,
        before_snapshot.envelope.updated_at
    );
    assert_eq!(
        after_snapshot.envelope.context_creation_sealed_at,
        Some(after_snapshot.envelope.updated_at)
    );
}

/// The pilot's write carries the grant it validated against: a different
/// grant at the write boundary is stale and writes nothing, the same one is
/// retained and re-bound to the pilot's scope.
#[test]
fn the_atomic_pilot_write_compares_and_carries_the_grant() {
    if !support::isolated("creation_grant::the_atomic_pilot_write_compares_and_carries_the_grant") {
        return;
    }
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let id = granted_task(&store, &[EXISTING, NEW], &[NEW]);
    let identity = state(&store, &id).identity();
    assert!(identity.is_some());

    let events = snapshot(&id).events;
    assert_eq!(
        pilot_write(&store, &id, None, &[EXISTING, NEW]),
        AtomicTaskMutationOutcome::Stale
    );
    assert_eq!(snapshot(&id).events, events);

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
    if !support::isolated(
        "creation_grant::revoke_and_same_scope_reauthorization_changes_the_pilot_generation",
    ) {
        return;
    }
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
    if !support::isolated("creation_grant::a_history_append_cannot_forge_a_grant") {
        return;
    }
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let id = granted_task(&store, &[EXISTING, NEW], &[]);
    let task = store
        .task
        .get_task(&id)
        .expect("get task")
        .expect("task exists");
    let forged = orbit_types::task::ContextCreationGrant::new(
        &id,
        scope(&[NEW]),
        &scope(&[EXISTING, NEW]),
        task.updated_at,
    );
    let refused = store.history.update_task_history(
        &id,
        TaskHistoryUpdateParams {
            actor: "agent".to_string(),
            append_history: vec![TaskHistoryEntry {
                at: Utc::now(),
                by: "agent".to_string(),
                event: CONTEXT_CREATION_AUTHORIZED_EVENT.to_string(),
                note: Some(forged.to_note()),
                from_status: None,
                to_status: None,
                operation_id: None,
                decision: None,
                surface: None,
                caller: None,
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
    if !support::isolated(
        "creation_grant::concurrent_scope_writes_leave_a_grant_bound_to_the_final_scope",
    ) {
        return;
    }
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
    let bundle = snapshot(&id);
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
