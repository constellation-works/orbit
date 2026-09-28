//! Migration tests for the legacy-tree import (ORB-10680).

use std::fs;

use orbit_types::record::FrictionStatus;

use super::super::{FrictionListFilter, FrictionUpdateParams};
use super::support::{add_params, at, friction_store, legacy_record, store};
use crate::workflow::friction::{
    export_workspace_frictions, import_with_canonicalizer, import_workspace_frictions,
};

#[test]
fn a_fresh_database_with_no_legacy_tree_imports_nothing() {
    let temp = tempfile::tempdir().expect("tempdir");
    let frictions = friction_store(temp.path(), "ws_one");

    let report = frictions.import_report().expect("report");

    assert_eq!(report.discovered, 0);
    assert_eq!(report.imported, 0);
    assert!(report.already_complete);
    assert!(
        frictions
            .list(&FrictionListFilter::default())
            .expect("list")
            .is_empty()
    );
}

/// A source that exists but is not a directory, or sits under a regular file,
/// is "nothing to import" just like a missing one.
#[test]
fn a_non_directory_source_imports_nothing() {
    let temp = tempfile::tempdir().expect("tempdir");
    let file_source = temp.path().join("ws_one");
    fs::write(&file_source, "not a directory\n").expect("file source");
    let shared = store(temp.path());

    for source in [file_source.clone(), file_source.join("nested")] {
        let report = import_workspace_frictions(&shared, "ws_one", &source)
            .expect("a non-directory source is not an import failure");
        assert_eq!(report.discovered, 0, "{}", source.display());
        assert!(!report.already_complete, "{}", source.display());
    }
}

/// A corpus root that cannot be resolved for any reason other than being
/// absent must fail the import without a completion marker; otherwise the
/// zero-record marker keeps the corpus from ever importing once access
/// returns (ORB-13608).
#[test]
fn a_corpus_root_io_failure_commits_no_marker_and_retries_after_recovery() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("ws_one");
    legacy_record(&source, "F2026-05-001", "codex", FrictionStatus::Open);
    let shared = store(temp.path());

    for kind in [
        std::io::ErrorKind::PermissionDenied,
        std::io::ErrorKind::Other,
    ] {
        let error = import_with_canonicalizer(&shared, "ws_one", &source, |_| {
            Err(std::io::Error::new(kind, "injected corpus-root failure"))
        })
        .expect_err("a corpus-root I/O failure must fail the import");
        let message = error.to_string();
        assert!(
            message.contains(&source.display().to_string())
                && message.contains("injected corpus-root failure"),
            "the error must name the corpus root and its cause: {message}"
        );
        assert_no_partial_import(temp.path(), "ws_one");
    }

    let report = import_workspace_frictions(&shared, "ws_one", &source)
        .expect("the same root imports once it resolves");
    assert_eq!(report.discovered, 1);
    assert_eq!(report.imported, 1);
    assert!(!report.already_complete);
}

/// The same guarantee against a real permission failure on an ancestor.
#[cfg(unix)]
#[test]
fn a_permission_denied_corpus_root_retries_after_access_is_restored() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().expect("tempdir");
    let parent = temp.path().join("locked");
    let source = parent.join("ws_one");
    legacy_record(&source, "F2026-05-001", "codex", FrictionStatus::Open);
    let shared = store(temp.path());

    fs::set_permissions(&parent, fs::Permissions::from_mode(0o000)).expect("lock parent");
    // A privileged runner ignores directory modes; there is nothing to deny.
    let denied = fs::canonicalize(&source).is_err();
    let result = import_workspace_frictions(&shared, "ws_one", &source);
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o755)).expect("unlock parent");
    if !denied {
        return;
    }

    let error = result.expect_err("an inaccessible corpus root must fail the import");
    assert!(
        error.to_string().contains(&source.display().to_string()),
        "the error must name the corpus root: {error}"
    );
    assert_no_partial_import(temp.path(), "ws_one");

    let report = import_workspace_frictions(&shared, "ws_one", &source)
        .expect("the same root imports once access is restored");
    assert_eq!(report.discovered, 1);
    assert_eq!(report.imported, 1);
}

/// A workspace whose legacy friction tree is reached through a symlinked
/// root is workspace-local configuration Orbit created, not attacker input;
/// it must not lose friction access (ORB-11992).
#[cfg(unix)]
#[test]
fn a_symlinked_legacy_root_constructs_and_serves_the_friction_store() {
    use orbit_common::test_fixtures::TEST_CODEX_MODEL;
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("tempdir");
    let real_source = temp.path().join("real_ws_one");
    let linked_source = temp.path().join("ws_one");
    legacy_record(&real_source, "F2026-05-001", "codex", FrictionStatus::Open);
    symlink(&real_source, &linked_source).expect("legacy root symlink");

    let frictions =
        crate::compose::workspace_friction_store(store(temp.path()), "ws_one", &linked_source)
            .expect("friction store must open behind a symlinked legacy root");

    assert_eq!(
        frictions
            .list(&FrictionListFilter::default())
            .expect("list")
            .len(),
        1,
        "the imported legacy record must be listed"
    );
    assert!(
        frictions.show("F2026-05-001").expect("show").is_some(),
        "the imported legacy record must be readable"
    );
    frictions
        .add(add_params(TEST_CODEX_MODEL, at(6, 0), &["tooling"]))
        .expect("add must still work behind a symlinked legacy root");
}

/// Every field the legacy envelope carried has to survive: record identity,
/// tags, body, timestamps, status, and both task links.
#[test]
fn a_successful_import_preserves_every_field() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("ws_one");
    legacy_record(&source, "F2026-05-001", "codex", FrictionStatus::Resolved);
    legacy_record(&source, "F2026-05-002", "claude", FrictionStatus::Open);

    let frictions = friction_store(temp.path(), "ws_one");
    let imported = frictions
        .show("F2026-05-001")
        .expect("show")
        .expect("record present");

    assert_eq!(
        imported.record.title.as_deref(),
        Some("Handle for F2026-05-001")
    );
    assert_eq!(imported.record.model, "codex");
    assert_eq!(imported.record.status, FrictionStatus::Resolved);
    assert_eq!(imported.record.tags, vec!["tooling".to_string()]);
    assert_eq!(imported.record.created_at, at(10, 12));
    assert_eq!(imported.record.resolved_at, Some(at(11, 9)));
    assert_eq!(imported.record.during_task.as_deref(), Some("ORB-00001"));
    assert_eq!(
        imported.record.resolved_by_task.as_deref(),
        Some("ORB-00002")
    );
    assert_eq!(imported.record.body, "Report body for F2026-05-001");
    // Import walks a canonicalized corpus root, so on macOS the stored path is
    // `/private/var/...` while `tempfile` may still present `$TMPDIR` as `/var`.
    let expected_path =
        fs::canonicalize(source.join("2026-05/F001.md")).expect("canonical evidence path");
    let imported_path = imported
        .path
        .as_ref()
        .map(|path| fs::canonicalize(path).expect("canonical imported evidence path"));
    assert_eq!(
        imported_path.as_deref(),
        Some(expected_path.as_path()),
        "an imported record keeps its evidence pointer"
    );
    assert_eq!(
        frictions
            .list(&FrictionListFilter::default())
            .expect("list")
            .len(),
        2
    );
}

#[test]
fn repeated_import_is_idempotent() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("ws_one");
    legacy_record(&source, "F2026-05-001", "codex", FrictionStatus::Open);

    let first = friction_store(temp.path(), "ws_one");
    let first_report = first.import_report().expect("first report");
    let second = friction_store(temp.path(), "ws_one");
    let second_report = second.import_report().expect("second report");

    assert_eq!(first_report.discovered, 1);
    assert!(
        first_report.already_complete,
        "reopened after the first open"
    );
    assert_eq!(second_report.discovered, 1);
    assert_eq!(second_report.imported, 1);
    assert_eq!(
        second.list(&FrictionListFilter::default()).unwrap().len(),
        1,
        "a second import must not duplicate the record"
    );
}

/// Two workspaces holding the same friction ID import independently and stay
/// separable afterwards.
#[test]
fn two_workspaces_import_the_same_friction_id_without_collision() {
    let temp = tempfile::tempdir().expect("tempdir");
    legacy_record(
        &temp.path().join("ws_one"),
        "F2026-05-001",
        "codex",
        FrictionStatus::Open,
    );
    legacy_record(
        &temp.path().join("ws_two"),
        "F2026-05-001",
        "claude",
        FrictionStatus::Resolved,
    );

    let shared = store(temp.path());
    let one = crate::compose::workspace_friction_store(
        shared.clone(),
        "ws_one",
        temp.path().join("ws_one"),
    )
    .expect("ws_one");
    let two =
        crate::compose::workspace_friction_store(shared, "ws_two", temp.path().join("ws_two"))
            .expect("ws_two");

    let first = one.show("F2026-05-001").unwrap().expect("ws_one record");
    let second = two.show("F2026-05-001").unwrap().expect("ws_two record");

    assert_eq!(first.record.model, "codex");
    assert_eq!(first.record.status, FrictionStatus::Open);
    assert_eq!(second.record.model, "claude");
    assert_eq!(second.record.status, FrictionStatus::Resolved);
}

#[test]
fn a_malformed_record_fails_the_import_closed() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("ws_one");
    legacy_record(&source, "F2026-05-001", "codex", FrictionStatus::Open);
    fs::write(source.join("2026-05/F002.md"), "no frontmatter here\n").expect("malformed record");

    let error = crate::compose::workspace_friction_store(store(temp.path()), "ws_one", &source)
        .err()
        .expect("malformed record must fail the import");

    assert!(error.to_string().contains("frontmatter"), "{error}");
    assert_no_partial_import(temp.path(), "ws_one");
}

#[test]
fn a_friction_id_claimed_twice_in_one_source_tree_fails_closed() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("ws_one");
    legacy_record(&source, "F2026-05-001", "codex", FrictionStatus::Open);
    // Same declared ID under a second filename in the same month.
    let duplicate = fs::read_to_string(source.join("2026-05/F001.md")).expect("read record");
    fs::write(source.join("2026-05/F002.md"), duplicate).expect("duplicate record");

    let error = crate::compose::workspace_friction_store(store(temp.path()), "ws_one", &source)
        .err()
        .expect("conflicting records must fail the import");

    assert!(error.to_string().contains("addresses"), "{error}");
    assert_no_partial_import(temp.path(), "ws_one");
}

/// An import that dies partway commits nothing: the next open sees an
/// unimported workspace and can retry from scratch once the source is fixed.
#[test]
fn an_import_interrupted_before_the_marker_leaves_no_partial_corpus() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("ws_one");
    legacy_record(&source, "F2026-05-001", "codex", FrictionStatus::Open);
    legacy_record(&source, "F2026-05-002", "codex", FrictionStatus::Open);
    // A third record aborts the walk after the first two were staged.
    fs::write(
        source.join("2026-05/F003.md"),
        "---\nnot: a record\n---\nbody\n",
    )
    .expect("aborting record");

    assert!(
        crate::compose::workspace_friction_store(store(temp.path()), "ws_one", &source).is_err()
    );
    assert_no_partial_import(temp.path(), "ws_one");

    fs::remove_file(source.join("2026-05/F003.md")).expect("repair source");
    let frictions = crate::compose::workspace_friction_store(store(temp.path()), "ws_one", &source)
        .expect("retry import");

    assert_eq!(
        frictions
            .list(&FrictionListFilter::default())
            .expect("list")
            .len(),
        2
    );
}

/// A marker written by a newer Orbit is refused rather than reinterpreted.
#[test]
fn an_import_marker_from_a_newer_schema_is_refused() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("ws_one");
    fs::create_dir_all(&source).expect("source root");
    let shared = store(temp.path());
    let canonical = fs::canonicalize(&source).expect("canonical source");
    shared
        .with_transaction(|tx| {
            tx.connection()
                .execute(
                    "INSERT INTO friction_import_state
                         (workspace_id, source_key, record_count, imported_count,
                          schema_version, completed_at)
                     VALUES ('ws_one', ?1, 0, 0, 99, '2026-05-01T00:00:00Z')",
                    rusqlite::params![canonical.to_string_lossy()],
                )
                .map_err(|error| orbit_common::OrbitError::Store(error.to_string()))?;
            Ok(())
        })
        .expect("seed newer marker");

    let error = crate::compose::workspace_friction_store(shared, "ws_one", &source)
        .err()
        .expect("newer import schema must be refused");

    assert!(error.to_string().contains("newer Orbit"), "{error}");
}

/// After the marker commits, SQLite is the sole live source: editing or even
/// deleting the legacy file changes nothing a reader sees.
#[test]
fn legacy_file_changes_cannot_affect_live_reads_after_import() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("ws_one");
    legacy_record(&source, "F2026-05-001", "codex", FrictionStatus::Open);
    let frictions = friction_store(temp.path(), "ws_one");

    fs::write(
        source.join("2026-05/F001.md"),
        "---\nid: F2026-05-001\nmodel: tampered\ncreated_at: 2026-05-10T12:00:00Z\n\
         status: resolved\ntags:\n- docs\n---\nTampered body\n",
    )
    .expect("tamper with the legacy file");
    legacy_record(&source, "F2026-05-002", "codex", FrictionStatus::Open);

    let reopened = friction_store(temp.path(), "ws_one");
    let live = reopened
        .show("F2026-05-001")
        .expect("show")
        .expect("record present");

    assert_eq!(live.record.model, "codex");
    assert_eq!(live.record.status, FrictionStatus::Open);
    assert_eq!(live.record.body, "Report body for F2026-05-001");
    assert_eq!(
        reopened.list(&FrictionListFilter::default()).unwrap().len(),
        1,
        "a file added after the import is not a live record"
    );
    assert!(
        frictions
            .show("F2026-05-001")
            .expect("show")
            .is_some_and(|stored| stored.record.model == "codex")
    );
}

/// Legacy files stay put as read-only evidence, and the corpus stays
/// inspectable through the export route.
#[test]
fn import_leaves_legacy_files_untouched_and_export_re_materializes_them() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("ws_one");
    legacy_record(&source, "F2026-05-001", "codex", FrictionStatus::Open);
    let original = fs::read_to_string(source.join("2026-05/F001.md")).expect("read original");

    let shared = store(temp.path());
    let frictions = crate::compose::workspace_friction_store(shared.clone(), "ws_one", &source)
        .expect("open friction store");
    frictions
        .update(
            "F2026-05-001",
            FrictionUpdateParams {
                status: Some(FrictionStatus::Triaged),
                tags: None,
                title: None,
                body: None,
                resolved_by_task: None,
                rehome_to: None,
                updated_at: at(12, 0),
            },
        )
        .expect("triage the live record");

    assert_eq!(
        fs::read_to_string(source.join("2026-05/F001.md")).expect("read after write"),
        original,
        "a live write must not rewrite the legacy evidence file"
    );

    let destination = temp.path().join("export");
    let exported =
        export_workspace_frictions(&shared, "ws_one", &destination).expect("export corpus");

    assert_eq!(exported, 1);
    let dumped = fs::read_to_string(destination.join("2026-05/F001.md")).expect("read export");
    assert!(dumped.contains("status: triaged"), "{dumped}");
    assert!(dumped.contains("Report body for F2026-05-001"), "{dumped}");
}

/// A `rehome_required` disposition set on the live record is part of the
/// record: exporting it and importing that export into a separate store must
/// carry the owning workspace along with every other field.
#[test]
fn export_then_import_preserves_the_rehome_disposition() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("ws_one");
    legacy_record(&source, "F2026-05-001", "codex", FrictionStatus::Resolved);

    let shared = store(temp.path());
    let frictions = crate::compose::workspace_friction_store(shared.clone(), "ws_one", &source)
        .expect("open friction store");
    frictions
        .update(
            "F2026-05-001",
            FrictionUpdateParams {
                status: None,
                tags: None,
                title: None,
                body: None,
                resolved_by_task: None,
                rehome_to: Some(Some("ws_owner".to_string())),
                updated_at: at(12, 0),
            },
        )
        .expect("record the rehome disposition");
    let live = frictions
        .show("F2026-05-001")
        .expect("show")
        .expect("live record")
        .record;
    assert_eq!(live.rehome_to.as_deref(), Some("ws_owner"));

    let exported_root = temp.path().join("export");
    assert_eq!(
        export_workspace_frictions(&shared, "ws_one", &exported_root).expect("export corpus"),
        1
    );

    let restored_root = temp.path().join("restored");
    fs::create_dir_all(&restored_root).expect("restored store root");
    let restored =
        crate::compose::workspace_friction_store(store(&restored_root), "ws_one", &exported_root)
            .expect("import the export into an isolated store")
            .show("F2026-05-001")
            .expect("show")
            .expect("imported record")
            .record;

    assert_eq!(restored, live);
}

fn assert_no_partial_import(root: &std::path::Path, workspace_id: &str) {
    let shared = store(root);
    let (records, markers): (i64, i64) = shared
        .with_read_connection(|conn| {
            let records = conn
                .query_row(
                    "SELECT COUNT(*) FROM friction_records WHERE workspace_id = ?1",
                    rusqlite::params![workspace_id],
                    |row| row.get(0),
                )
                .map_err(|error| orbit_common::OrbitError::Store(error.to_string()))?;
            let markers = conn
                .query_row(
                    "SELECT COUNT(*) FROM friction_import_state WHERE workspace_id = ?1",
                    rusqlite::params![workspace_id],
                    |row| row.get(0),
                )
                .map_err(|error| orbit_common::OrbitError::Store(error.to_string()))?;
            Ok((records, markers))
        })
        .expect("inspect import state");

    assert_eq!(records, 0, "a failed import must leave no records behind");
    assert_eq!(
        markers, 0,
        "a failed import must leave no completion marker"
    );
}
