// Migrated from file/friction_store.rs per ORB-00231
use super::super::*;
use chrono::Utc;
use orbit_types::record::FrictionStatus;

#[test]
fn hub_migration_publishes_complete_tree_and_is_idempotent() {
    let temp = tempfile::tempdir().expect("tempdir");
    let legacy = temp.path().join("legacy");
    fs::create_dir_all(legacy.join("2026-07")).expect("legacy month");
    fs::write(legacy.join("tags.yaml"), "tooling: Tools\n").expect("taxonomy");
    fs::write(legacy.join("2026-07/F001.md"), "record\n").expect("record");

    let canonical = prepare_hub_friction_root(temp.path(), "ws_test", Some(&legacy))
        .expect("publish migration");
    assert_eq!(
        fs::read(canonical.join("2026-07/F001.md")).unwrap(),
        b"record\n"
    );
    assert_eq!(
        prepare_hub_friction_root(temp.path(), "ws_test", Some(&legacy)).unwrap(),
        canonical
    );
    assert_eq!(
        readable_hub_friction_root(temp.path(), "ws_test", Some(&legacy)).unwrap(),
        canonical
    );
}

#[test]
fn hub_migration_accepts_identical_interrupted_publish_and_commits_marker() {
    let temp = tempfile::tempdir().expect("tempdir");
    let legacy = temp.path().join("legacy");
    let canonical = canonical_hub_friction_root(temp.path(), "ws_test").unwrap();
    fs::create_dir_all(&legacy).unwrap();
    fs::create_dir_all(&canonical).unwrap();
    fs::write(legacy.join("tags.yaml"), "same\n").unwrap();
    fs::write(canonical.join("tags.yaml"), "same\n").unwrap();

    assert_eq!(
        readable_hub_friction_root(temp.path(), "ws_test", Some(&legacy)).unwrap(),
        legacy
    );
    prepare_hub_friction_root(temp.path(), "ws_test", Some(&legacy)).unwrap();
    assert_eq!(
        readable_hub_friction_root(temp.path(), "ws_test", Some(&legacy)).unwrap(),
        canonical
    );
}

#[test]
fn checkoutless_prepare_does_not_commit_an_unknown_legacy_migration() {
    let temp = tempfile::tempdir().expect("tempdir");
    let canonical = prepare_hub_friction_root(temp.path(), "ws_test", None)
        .expect("checkoutless canonical root");
    let marker = canonical
        .parent()
        .unwrap()
        .join(".migration-markers/ws_test.complete");
    assert!(canonical.is_dir());
    assert!(!marker.exists());

    let legacy = temp.path().join("legacy");
    fs::create_dir_all(&legacy).unwrap();
    fs::write(legacy.join("tags.yaml"), "legacy: state\n").unwrap();
    prepare_hub_friction_root(temp.path(), "ws_test", Some(&legacy))
        .expect("later known legacy migration");

    assert!(marker.exists());
    assert_eq!(
        fs::read(canonical.join("tags.yaml")).unwrap(),
        b"legacy: state\n"
    );
}

#[test]
fn hub_migration_conflict_fails_closed_and_preserves_legacy_reads() {
    let temp = tempfile::tempdir().expect("tempdir");
    let legacy = temp.path().join("legacy");
    let canonical = canonical_hub_friction_root(temp.path(), "ws_test").unwrap();
    fs::create_dir_all(&legacy).unwrap();
    fs::create_dir_all(&canonical).unwrap();
    fs::write(legacy.join("tags.yaml"), "legacy\n").unwrap();
    fs::write(canonical.join("tags.yaml"), "different\n").unwrap();

    let error = prepare_hub_friction_root(temp.path(), "ws_test", Some(&legacy))
        .expect_err("conflict must fail");
    assert!(error.to_string().contains("migration conflict"));
    assert_eq!(
        readable_hub_friction_root(temp.path(), "ws_test", Some(&legacy)).unwrap(),
        legacy
    );
    assert_eq!(
        fs::read(canonical.join("tags.yaml")).unwrap(),
        b"different\n"
    );
}

/// The legacy layout is still the shape the importer reads and the export
/// route writes, so the round trip has to stay lossless.
#[test]
fn a_record_round_trips_through_the_legacy_markdown_layout() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("2026-05/F001.md");
    let record = FrictionRecord {
        id: "F2026-05-001".to_string(),
        title: Some("Queued runs never reach a worker".to_string()),
        model: "codex".to_string(),
        created_at: Utc::now(),
        status: FrictionStatus::Triaged,
        tags: vec!["tooling".to_string()],
        resolved_at: None,
        during_task: Some("ORB-00001".to_string()),
        resolved_by_task: None,
        body: "The worker exited before claiming the run.".to_string(),
    };

    write_record_at(&path, &record).expect("write record");
    let stored = read_record_at(&path).expect("read record");

    assert_eq!(stored.record.id, record.id);
    assert_eq!(stored.record.title, record.title);
    assert_eq!(stored.record.status, record.status);
    assert_eq!(stored.record.tags, record.tags);
    assert_eq!(stored.record.during_task, record.during_task);
    assert_eq!(stored.record.body, record.body);
    assert_eq!(stored.path.as_deref(), Some(path.as_path()));
}

/// A record written before `title` existed still parses; its handle comes from
/// derivation on read, so no rewrite pass is owed before import.
#[test]
fn a_record_without_a_title_field_still_parses() {
    let temp = tempfile::tempdir().expect("tempdir");
    let month = temp.path().join("2026-05");
    fs::create_dir_all(&month).expect("month dir");
    let path = month.join("F001.md");
    fs::write(
        &path,
        "---\nid: F2026-05-001\nmodel: codex\ncreated_at: 2026-05-17T04:05:00Z\n\
         status: open\ntags:\n- tooling\n---\nThe worker exited before claiming the run.\n",
    )
    .expect("legacy record");

    let stored = read_record_at(&path).expect("read record");

    assert_eq!(stored.record.title, None);
    assert_eq!(
        stored.record.body,
        "The worker exited before claiming the run."
    );
}

/// The taxonomy is configuration, not record state: ORB-10680 left it a file.
#[test]
fn the_tag_taxonomy_is_seeded_and_read_from_the_workspace_file() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();

    let path = ensure_default_tag_taxonomy(root).expect("seed taxonomy");
    assert!(path.ends_with(TAGS_FILENAME));
    assert!(load_tag_taxonomy(root).expect("load").contains("tooling"));

    fs::write(root.join(TAGS_FILENAME), "surprise-tag: allowed\n").expect("rewrite taxonomy");
    let taxonomy = load_tag_taxonomy(root).expect("reload");
    assert!(taxonomy.contains("surprise-tag"));
    assert!(!taxonomy.contains("tooling"));
}

/// The record walk is what the importer streams; it must find every month's
/// records in a stable order and ignore configuration files at the root.
#[test]
fn the_record_walk_lists_month_records_in_order() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();
    fs::create_dir_all(root.join("2026-05")).unwrap();
    fs::create_dir_all(root.join("2026-06")).unwrap();
    fs::write(root.join("tags.yaml"), "tooling: Tools\n").unwrap();
    fs::write(root.join("2026-06/F001.md"), "later\n").unwrap();
    fs::write(root.join("2026-05/F002.md"), "second\n").unwrap();
    fs::write(root.join("2026-05/F001.md"), "first\n").unwrap();

    let paths = friction_record_paths(root).expect("walk");
    let canonical_root = fs::canonicalize(root).expect("canonical root");

    assert_eq!(
        paths,
        vec![
            canonical_root.join("2026-05/F001.md"),
            canonical_root.join("2026-05/F002.md"),
            canonical_root.join("2026-06/F001.md"),
        ]
    );
}

/// A legacy tree reached through a symlinked root (a symlinked `$HOME`, a
/// relocated data dir, macOS's `/var` -> `/private/var`) is workspace-local
/// configuration, not attacker-controlled input; the walk must resolve it
/// rather than fail workspace-wide friction access closed.
#[cfg(unix)]
#[test]
fn the_record_walk_follows_a_symlinked_root() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("tempdir");
    let real_root = temp.path().join("real");
    let linked_root = temp.path().join("linked");
    fs::create_dir_all(real_root.join("2026-05")).expect("real root");
    fs::write(real_root.join("2026-05/F001.md"), "record\n").expect("record");
    symlink(&real_root, &linked_root).expect("root symlink");

    let paths = friction_record_paths(&linked_root).expect("symlinked root must resolve");
    let canonical_root = fs::canonicalize(&real_root).expect("canonical real root");

    assert_eq!(paths, vec![canonical_root.join("2026-05/F001.md")]);
}

#[cfg(unix)]
#[test]
fn the_record_walk_ignores_symlinked_months_and_records() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("frictions");
    let outside = temp.path().join("outside");
    fs::create_dir_all(root.join("2026-05")).expect("month dir");
    fs::create_dir_all(outside.join("2026-06")).expect("outside month dir");
    fs::write(root.join("2026-05/F001.md"), "inside\n").expect("inside record");
    fs::write(outside.join("2026-06/F002.md"), "outside\n").expect("outside record");
    symlink(outside.join("2026-06"), root.join("2026-06")).expect("month symlink");
    symlink(
        outside.join("2026-06/F002.md"),
        root.join("2026-05/F002.md"),
    )
    .expect("record symlink");

    let paths = friction_record_paths(&root).expect("walk");
    let canonical_root = fs::canonicalize(&root).expect("canonical root");

    assert_eq!(paths, vec![canonical_root.join("2026-05/F001.md")]);
}

/// A symlinked root must resolve (`the_record_walk_follows_a_symlinked_root`)
/// without weakening containment: a month entry inside that root which is
/// itself a symlink escaping the resolved corpus must still be refused.
#[cfg(unix)]
#[test]
fn a_symlinked_root_still_refuses_an_escaping_record_symlink() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("tempdir");
    let real_root = temp.path().join("real");
    let linked_root = temp.path().join("linked");
    let outside = temp.path().join("outside");
    fs::create_dir_all(real_root.join("2026-05")).expect("month dir");
    fs::create_dir_all(outside.join("2026-06")).expect("outside month dir");
    fs::write(real_root.join("2026-05/F001.md"), "inside\n").expect("inside record");
    fs::write(outside.join("2026-06/F002.md"), "outside\n").expect("outside record");
    symlink(outside.join("2026-06"), real_root.join("2026-06")).expect("month symlink");
    symlink(&real_root, &linked_root).expect("root symlink");

    let paths = friction_record_paths(&linked_root).expect("symlinked root must resolve");
    let canonical_root = fs::canonicalize(&real_root).expect("canonical real root");

    assert_eq!(paths, vec![canonical_root.join("2026-05/F001.md")]);
}
