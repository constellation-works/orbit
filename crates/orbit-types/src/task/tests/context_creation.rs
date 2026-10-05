//! Which grant records a task's history may trust for its current scope, and
//! the record a scope write must leave behind.

use crate::task::{
    CONTEXT_CREATION_AUTHORIZED_EVENT, ContextCreationGrant, ContextCreationState,
    MAX_CONTEXT_CREATION_SELECTORS,
};

const TASK: &str = "ORB-00001";

fn scope(selectors: &[&str]) -> Vec<String> {
    selectors
        .iter()
        .map(|selector| selector.to_string())
        .collect()
}

fn resolve(context_files: &[String], notes: &[String]) -> ContextCreationState {
    let events = notes
        .iter()
        .map(|note| (CONTEXT_CREATION_AUTHORIZED_EVENT, Some(note.as_str())))
        .collect::<Vec<_>>();
    ContextCreationState::resolve(TASK, context_files, events.into_iter())
}

#[test]
fn only_the_latest_grant_bound_to_this_task_and_scope_is_current() {
    let files = scope(&["file:a.rs", "file:b.rs"]);
    let grant = ContextCreationGrant::new(TASK, scope(&["file:b.rs"]), &files);
    let reordered = scope(&["file:b.rs", "file:a.rs"]);
    assert_eq!(
        resolve(&reordered, &[grant.to_note()]),
        ContextCreationState::Current(grant.clone()),
        "the scope binding ignores order"
    );

    let revoked = ContextCreationGrant::new(TASK, Vec::new(), &files);
    assert_eq!(
        resolve(&files, &[grant.to_note(), revoked.to_note()]).selectors(),
        [] as [String; 0]
    );

    let mut unsorted = ContextCreationGrant::new(TASK, scope(&["file:a.rs", "file:b.rs"]), &files);
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
            ContextCreationGrant::new("ORB-00002", scope(&["file:b.rs"]), &files).to_note(),
        ),
        (
            "bound to another scope",
            ContextCreationGrant::new(TASK, scope(&["file:b.rs"]), &scope(&["file:b.rs"]))
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
    let current = ContextCreationState::Current(ContextCreationGrant::new(
        TASK,
        scope(&["file:b.rs"]),
        &files,
    ));

    assert_eq!(
        current.next_grant(TASK, &files, &[]).unwrap(),
        None,
        "an unchanged record is not rewritten"
    );

    let widened = scope(&["file:a.rs", "file:b.rs", "file:c.rs"]);
    let next = current
        .next_grant(TASK, &widened, &scope(&["file:c.rs"]))
        .unwrap()
        .unwrap();
    assert_eq!(
        next,
        ContextCreationGrant::new(TASK, scope(&["file:b.rs", "file:c.rs"]), &widened)
    );

    let narrowed = scope(&["file:a.rs"]);
    let revoked = current.next_grant(TASK, &narrowed, &[]).unwrap().unwrap();
    assert!(
        revoked.selectors.is_empty(),
        "a dropped selector's grant is revoked explicitly"
    );

    assert_eq!(
        ContextCreationState::Absent
            .next_grant(TASK, &files, &[])
            .unwrap(),
        None
    );
    let void = ContextCreationState::Void
        .next_grant(TASK, &files, &[])
        .unwrap()
        .unwrap();
    assert!(
        void.selectors.is_empty(),
        "a void grant is replaced, never carried"
    );

    assert!(
        current
            .next_grant(TASK, &files, &scope(&["file:c.rs"]))
            .is_err(),
        "authorized outside the written scope"
    );
    let many = (0..=MAX_CONTEXT_CREATION_SELECTORS)
        .map(|index| format!("file:{index:03}.rs"))
        .collect::<Vec<_>>();
    assert!(
        ContextCreationState::Absent
            .next_grant(TASK, &many, &many)
            .is_err(),
        "over the cap"
    );
}
