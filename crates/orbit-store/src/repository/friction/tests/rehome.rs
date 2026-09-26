//! Re-homing: the `rehome_required` disposition and the move into the owning
//! workspace (ORB-13024).

use std::path::Path;

use chrono::{DateTime, Utc};
use orbit_common::test_fixtures::TEST_CODEX_MODEL;
use orbit_common::{NotFoundKind, OrbitError};
use orbit_types::record::FrictionStatus;

use super::super::{FrictionListFilter, FrictionRehomeParams, FrictionStore, FrictionUpdateParams};
use super::support::{add_params, at, store};

fn open(root: &Path, workspace_id: &str) -> FrictionStore {
    FrictionStore::open(store(root), workspace_id, root.join(workspace_id))
        .expect("open friction store")
}

fn update(
    rehome_to: Option<Option<String>>,
    status: Option<FrictionStatus>,
) -> FrictionUpdateParams {
    FrictionUpdateParams {
        status,
        tags: None,
        title: None,
        body: None,
        resolved_by_task: None,
        rehome_to,
        updated_at: at(6, 0),
    }
}

fn rehome_params(root: &Path, target: &str, rehomed_at: DateTime<Utc>) -> FrictionRehomeParams {
    FrictionRehomeParams {
        target_workspace_id: target.to_string(),
        target_files_root: root.join(target),
        target_label: format!("label_{target}"),
        source_label: "label_source".to_string(),
        rehomed_at,
    }
}

#[test]
fn update_records_and_clears_the_rehome_disposition_without_resolving() {
    let temp = tempfile::tempdir().expect("tempdir");
    let frictions = open(temp.path(), "ws_product");
    let id = frictions
        .add(add_params(TEST_CODEX_MODEL, at(4, 9), &["tooling"]))
        .expect("add")
        .record
        .id;

    frictions
        .update(&id, update(Some(Some("ws_orbit".to_string())), None))
        .expect("record the owning workspace");
    let recorded = frictions.show(&id).expect("show").expect("present");
    assert_eq!(recorded.record.rehome_to.as_deref(), Some("ws_orbit"));
    assert_eq!(
        recorded.record.status,
        FrictionStatus::Open,
        "the disposition records ownership; it does not claim a fix"
    );

    // An unrelated update keeps the disposition.
    frictions
        .update(&id, update(None, Some(FrictionStatus::Triaged)))
        .expect("triage");
    assert_eq!(
        frictions
            .show(&id)
            .unwrap()
            .unwrap()
            .record
            .rehome_to
            .as_deref(),
        Some("ws_orbit")
    );

    frictions
        .update(&id, update(Some(None), None))
        .expect("clear the disposition");
    assert_eq!(frictions.show(&id).unwrap().unwrap().record.rehome_to, None);
}

#[test]
fn rehome_moves_the_record_and_resolves_the_source_with_a_pointer() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = open(temp.path(), "ws_product");
    let owner = open(temp.path(), "ws_platform");
    std::fs::create_dir_all(temp.path().join("ws_product")).expect("source files root");
    std::fs::write(
        temp.path().join("ws_product").join("tags.yaml"),
        "tooling: tools\nproduct-only: a tag the owner does not define\n",
    )
    .expect("source taxonomy");
    // The owner already holds a May record, so the move must allocate there.
    owner
        .add(add_params(TEST_CODEX_MODEL, at(2, 0), &["docs"]))
        .expect("existing owner record");

    let mut params = add_params(TEST_CODEX_MODEL, at(4, 9), &["tooling", "product-only"]);
    params.title = Some("Sandbox denies gh config reads".to_string());
    params.body = "## What happened\n\ngh auth status failed.".to_string();
    params.during_task = Some("DANI-10691".to_string());
    let original = source.add(params).expect("add").record;
    source
        .update(&original.id, update(None, Some(FrictionStatus::Triaged)))
        .expect("triage");

    let outcome = source
        .rehome(
            &original.id,
            rehome_params(temp.path(), "ws_platform", at(20, 0)),
        )
        .expect("rehome");

    let moved = owner
        .show(&outcome.target.record.id)
        .expect("show moved")
        .expect("the owner holds the moved record");
    assert_eq!(moved.record.id, "F2026-05-002");
    assert_eq!(moved.record.title, original.title);
    assert_eq!(moved.record.model, original.model);
    assert_eq!(moved.record.created_at, original.created_at);
    assert_eq!(moved.record.during_task, original.during_task);
    assert_eq!(moved.record.status, FrictionStatus::Triaged);
    assert_eq!(moved.record.tags, vec!["tooling".to_string()]);
    assert_eq!(moved.record.rehome_to, None);
    assert!(moved.record.body.starts_with(&original.body));
    assert!(
        moved.record.body.contains("label_source") && moved.record.body.contains(&original.id),
        "the moved record names where it came from: {}",
        moved.record.body
    );
    assert_eq!(outcome.dropped_tags, vec!["product-only".to_string()]);

    let resolved = source
        .show(&original.id)
        .expect("show source")
        .expect("the source record stays as the forwarding record");
    assert_eq!(resolved.record.status, FrictionStatus::Resolved);
    assert_eq!(resolved.record.resolved_at, Some(at(20, 0)));
    assert_eq!(
        resolved.record.rehome_to.as_deref(),
        Some("label_ws_platform")
    );
    assert!(resolved.record.body.starts_with(&original.body));
    assert!(
        resolved.record.body.contains("F2026-05-002"),
        "the source points at the new id: {}",
        resolved.record.body
    );
}

#[test]
fn rehome_refuses_what_it_cannot_move_and_writes_nothing() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = open(temp.path(), "ws_product");
    let owner = open(temp.path(), "ws_platform");
    let open_id = source
        .add(add_params(TEST_CODEX_MODEL, at(4, 9), &["tooling"]))
        .expect("add open")
        .record
        .id;
    let resolved_id = source
        .add(add_params(TEST_CODEX_MODEL, at(4, 10), &["tooling"]))
        .expect("add resolved")
        .record
        .id;
    source
        .update(&resolved_id, update(None, Some(FrictionStatus::Resolved)))
        .expect("resolve");

    let cases = [
        (
            resolved_id.as_str(),
            "ws_platform",
            "an already resolved record",
        ),
        (
            open_id.as_str(),
            "ws_product",
            "the workspace that already owns it",
        ),
        (
            "not-a-friction-id",
            "ws_platform",
            "a malformed friction id",
        ),
    ];
    for (id, target, case) in cases {
        let error = source
            .rehome(id, rehome_params(temp.path(), target, at(20, 0)))
            .expect_err(case);
        assert!(
            matches!(error, OrbitError::InvalidInput(_)),
            "{case}: {error:?}"
        );
    }

    let missing_error = source
        .rehome(
            "F2026-05-099",
            rehome_params(temp.path(), "ws_platform", at(20, 0)),
        )
        .expect_err("a record that does not exist");
    assert!(
        matches!(
            missing_error,
            OrbitError::NotFound {
                kind: NotFoundKind::Friction,
                ref id,
            } if id == "F2026-05-099"
        ),
        "{missing_error:?}"
    );

    assert!(
        owner
            .list(&FrictionListFilter::default())
            .unwrap()
            .is_empty()
    );
    let untouched = source.show(&open_id).unwrap().unwrap();
    assert_eq!(untouched.record.status, FrictionStatus::Open);
    assert_eq!(untouched.record.rehome_to, None);
}

/// The copy and the source's resolution are one transaction: when the owner
/// cannot allocate an ID, the source stays open and unmodified.
#[test]
fn a_failed_move_leaves_the_source_open() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = open(temp.path(), "ws_product");
    let owner = open(temp.path(), "ws_platform");
    let id = source
        .add(add_params(TEST_CODEX_MODEL, at(4, 9), &["tooling"]))
        .expect("add")
        .record
        .id;
    owner
        .add(add_params(TEST_CODEX_MODEL, at(1, 0), &["tooling"]))
        .expect("owner record");
    store(temp.path())
        .with_transaction(|tx| {
            tx.connection()
                .execute(
                    "UPDATE friction_records SET seq = 999 WHERE workspace_id = 'ws_platform'",
                    [],
                )
                .map_err(|error| OrbitError::Store(error.to_string()))?;
            Ok(())
        })
        .expect("exhaust the owner's May counter");

    source
        .rehome(&id, rehome_params(temp.path(), "ws_platform", at(20, 0)))
        .expect_err("the owner's month is full");

    let untouched = source.show(&id).unwrap().unwrap();
    assert_eq!(untouched.record.status, FrictionStatus::Open);
    assert_eq!(untouched.record.rehome_to, None);
    assert_eq!(owner.list(&FrictionListFilter::default()).unwrap().len(), 1);
}
