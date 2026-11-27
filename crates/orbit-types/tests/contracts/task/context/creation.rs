//! Which grant records a task's history may trust for its current scope, and
//! the record a scope write must leave behind.

use chrono::{DateTime, Utc};
use orbit_types::task::{
    CONTEXT_CREATION_AUTHORIZED_EVENT, ContextCreationGrant, ContextCreationState,
    MAX_CONTEXT_CREATION_SELECTORS,
};

const TASK: &str = "ORB-00001";

fn revision() -> DateTime<Utc> {
    DateTime::from_timestamp(0, 0).unwrap()
}

fn scope(selectors: &[&str]) -> Vec<String> {
    selectors
        .iter()
        .map(|selector| selector.to_string())
        .collect()
}

fn resolve(context_files: &[String], notes: &[String]) -> ContextCreationState {
    resolve_at(context_files, revision(), Some(revision()), notes)
}

fn resolve_at(
    context_files: &[String],
    updated_at: DateTime<Utc>,
    sealed_at: Option<DateTime<Utc>>,
    notes: &[String],
) -> ContextCreationState {
    let events = notes
        .iter()
        .map(|note| (CONTEXT_CREATION_AUTHORIZED_EVENT, Some(note.as_str())))
        .collect::<Vec<_>>();
    ContextCreationState::resolve(
        TASK,
        context_files,
        updated_at,
        sealed_at,
        events.into_iter(),
    )
}

#[test]
fn only_the_latest_grant_bound_to_this_task_and_scope_is_current() {
    let files = scope(&["file:a.rs", "file:b.rs"]);
    let grant = ContextCreationGrant::new(TASK, scope(&["file:b.rs"]), &files, revision());
    let reordered = scope(&["file:b.rs", "file:a.rs"]);
    assert_eq!(
        resolve(&reordered, &[grant.to_note()]),
        ContextCreationState::Current(grant.clone()),
        "the scope binding ignores order"
    );

    let revoked = ContextCreationGrant::new(TASK, Vec::new(), &files, revision());
    assert_eq!(
        resolve(&files, &[grant.to_note(), revoked.to_note()]).selectors(),
        [] as [String; 0]
    );

    let mut unsorted =
        ContextCreationGrant::new(TASK, scope(&["file:a.rs", "file:b.rs"]), &files, revision());
    unsorted.selectors.reverse();
    let mut over_cap = grant.clone();
    over_cap.selectors = (0..=MAX_CONTEXT_CREATION_SELECTORS)
        .map(|index| format!("file:{index:03}.rs"))
        .collect();
    let mut outside_scope = grant.clone();
    outside_scope.selectors = scope(&["file:c.rs"]);
    let mut future_version = grant.clone();
    future_version.version += 1;
    let void = [
        (
            "copied from another task",
            ContextCreationGrant::new("ORB-00002", scope(&["file:b.rs"]), &files, revision())
                .to_note(),
        ),
        (
            "bound to another scope",
            ContextCreationGrant::new(
                TASK,
                scope(&["file:b.rs"]),
                &scope(&["file:b.rs"]),
                revision(),
            )
            .to_note(),
        ),
        ("unsorted", unsorted.to_note()),
        ("over the cap", over_cap.to_note()),
        ("outside the scope", outside_scope.to_note()),
        ("another version", future_version.to_note()),
        ("malformed", "authorize file:b.rs".to_string()),
        (
            "unknown field",
            grant.to_note().replacen('{', "{\"blanket\":true,", 1),
        ),
    ];
    for (case, note) in void {
        assert_eq!(
            resolve(&files, &[grant.to_note(), note]),
            ContextCreationState::Void,
            "{case}: a later untrusted record must void the grant, not fall back to an older one"
        );
    }
}

#[test]
fn a_scope_write_retains_kept_grants_adds_new_ones_and_rebinds() {
    let files = scope(&["file:a.rs", "file:b.rs"]);
    let mut grant = ContextCreationGrant::new(TASK, scope(&["file:b.rs"]), &files, revision());
    grant.generation = Some("EV-0002".to_string());
    let current = ContextCreationState::Current(grant);

    assert_eq!(
        current.next_grant(TASK, &files, &[], revision()).unwrap(),
        None,
        "an unchanged record is not rewritten"
    );

    let widened = scope(&["file:a.rs", "file:b.rs", "file:c.rs"]);
    let next = current
        .next_grant(TASK, &widened, &scope(&["file:c.rs"]), revision())
        .unwrap()
        .unwrap();
    assert_eq!(
        next,
        ContextCreationGrant::new(
            TASK,
            scope(&["file:b.rs", "file:c.rs"]),
            &widened,
            revision(),
        )
    );

    let narrowed = scope(&["file:a.rs"]);
    let revoked = current
        .next_grant(TASK, &narrowed, &[], revision())
        .unwrap()
        .unwrap();
    assert!(
        revoked.selectors.is_empty(),
        "a dropped selector's grant is revoked explicitly"
    );

    assert_eq!(
        ContextCreationState::Absent
            .next_grant(TASK, &files, &[], revision())
            .unwrap(),
        None
    );
    let void = ContextCreationState::Void
        .next_grant(TASK, &files, &[], revision())
        .unwrap()
        .unwrap();
    assert!(
        void.selectors.is_empty(),
        "a void grant is replaced, never carried"
    );

    assert!(
        current
            .next_grant(TASK, &files, &scope(&["file:c.rs"]), revision())
            .is_err(),
        "authorized outside the written scope"
    );
    let many = (0..=MAX_CONTEXT_CREATION_SELECTORS)
        .map(|index| format!("file:{index:03}.rs"))
        .collect::<Vec<_>>();
    assert!(
        ContextCreationState::Absent
            .next_grant(TASK, &many, &many, revision())
            .is_err(),
        "over the cap"
    );
}

#[test]
fn refreshing_the_revision_seal_keeps_semantic_identity_and_generation() {
    let files = scope(&["file:a.rs", "file:b.rs"]);
    let mut initial = ContextCreationGrant::new(TASK, scope(&["file:b.rs"]), &files, revision());
    initial.generation = Some("EV-0002".to_string());
    let state = ContextCreationState::Current(initial.clone());
    let refreshed_at = revision() + chrono::Duration::seconds(1);

    // An unchanged grant emits no new history row when only `updated_at` advances.
    assert_eq!(
        state.next_grant(TASK, &files, &[], refreshed_at).unwrap(),
        None
    );

    // Carrying the envelope seal forward keeps the existing grant current with
    // the exact same selectors, generation and identity.
    let note = initial.to_note();
    assert_eq!(
        resolve_at(
            &files,
            refreshed_at,
            Some(refreshed_at),
            std::slice::from_ref(&note)
        ),
        ContextCreationState::Current(initial.clone())
    );

    // A stale or missing seal fails closed once `updated_at` advances.
    assert_eq!(
        resolve_at(
            &files,
            refreshed_at,
            Some(revision()),
            std::slice::from_ref(&note)
        ),
        ContextCreationState::Void
    );
    assert_eq!(
        resolve_at(&files, refreshed_at, None, std::slice::from_ref(&note)),
        ContextCreationState::Void
    );

    // Legacy bundles with no envelope seal resolve via `grant.updated_at`.
    assert_eq!(
        resolve_at(&files, revision(), None, std::slice::from_ref(&note)),
        ContextCreationState::Current(initial.clone())
    );

    // A grant missing its recorded `updated_at` stays void even if sealed.
    let mut without_updated_at = initial.clone();
    without_updated_at.updated_at = None;
    assert_eq!(
        resolve_at(
            &files,
            refreshed_at,
            Some(refreshed_at),
            &[without_updated_at.to_note()],
        ),
        ContextCreationState::Void
    );

    let mut next_generation = initial.clone();
    next_generation.generation = Some("EV-0003".to_string());
    assert_ne!(next_generation.identity(), initial.identity());
}
