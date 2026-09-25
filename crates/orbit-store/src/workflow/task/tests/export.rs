use std::fs;

use chrono::Utc;
use orbit_types::task::{
    ArtifactManifestV2, TASK_ARTIFACT_SCHEMA_VERSION, TaskEventRowV2, TaskStatus,
};
use tempfile::TempDir;

use crate::driver::file::task_bundle::read_bundle_at;

use super::*;

#[test]
fn export_ids_subset_and_rejects_unknown() {
    let src = TempDir::new().unwrap();
    let archive = src.path().join("subset.tar.zst");
    let ws = "orbit-src-aaaaaa";
    build_source_archive(src.path(), ws, &archive);
    let registry = open_registry(src.path());

    let subset = src.path().join("only-child.tar.zst");
    let outcome = export_tasks(
        &registry,
        ws,
        ExportSelection::Ids(vec!["ORB-00001".to_string()]),
        &subset,
        exported_at(),
    )
    .unwrap();
    assert_eq!(outcome.task_ids, vec!["ORB-00001"]);

    let err = export_tasks(
        &registry,
        ws,
        ExportSelection::Ids(vec!["ORB-09999".to_string()]),
        &src.path().join("bad.tar.zst"),
        exported_at(),
    )
    .unwrap_err();
    assert!(format!("{err}").contains("not registered"));
}

#[test]
fn export_waits_for_a_transition_and_imports_the_settled_bundle() {
    use std::sync::Barrier;
    use std::time::Duration;

    let src = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();
    let archive = src.path().join("tasks.tar.zst");
    let ws = "export-transition-abcdef";
    let registry = open_registry(src.path());
    let binding = bind(&registry, src.path(), ws);
    let store = bundle_store(&registry, &binding);
    let bundle = make_bundle("ORB-00000", "transition", Vec::new());
    seed(&store, &registry, ws, &bundle);

    let event_appended = Barrier::new(2);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            store
                .with_bundle_write_lock("ORB-00000", || {
                    let current = store.read_bundle("ORB-00000")?;
                    let event = TaskEventRowV2 {
                        schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
                        event_id: "EV-0002".to_string(),
                        at: Utc::now(),
                        by: "codex".to_string(),
                        event_type: "status_changed".to_string(),
                        note: None,
                        from_status: Some(TaskStatus::Backlog),
                        to_status: Some(TaskStatus::InProgress),
                    };
                    store.append_event("ORB-00000", &event)?;
                    event_appended.wait();
                    std::thread::sleep(Duration::from_millis(100));

                    let mut envelope = current.envelope;
                    envelope.status = TaskStatus::InProgress;
                    envelope.updated_at = Utc::now();
                    store.rewrite_envelope("ORB-00000", &envelope)
                })
                .expect("finish transition");
        });

        event_appended.wait();
        export_tasks(&registry, ws, ExportSelection::All, &archive, exported_at())
            .expect("export waits for the settled transition");
    });

    let target = open_registry(dst.path());
    let outcome =
        import_tasks(&target, &archive, None, ImportConflictPolicy::Fail).expect("import");
    assert_eq!(outcome.tasks[0].action, ImportAction::Kept);
    let imported = read_bundle_at(
        &target
            .canonical_task_bundle_path(ws, "ORB-00000")
            .expect("canonical path"),
    )
    .expect("read imported bundle");
    assert_eq!(imported.envelope.status, TaskStatus::InProgress);
    assert_eq!(
        imported.events.last().and_then(|event| event.to_status),
        Some(TaskStatus::InProgress)
    );
}

#[test]
fn export_excludes_pending_sidecar_but_keeps_dotfile_artifacts() {
    let src = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();
    let archive = src.path().join("tasks.tar.zst");
    let ws = "export-sidecars-abcdef";
    let registry = open_registry(src.path());
    let binding = bind(&registry, src.path(), ws);
    let store = bundle_store(&registry, &binding);
    seed(
        &store,
        &registry,
        ws,
        &make_bundle("ORB-00000", "dotfile artifact", Vec::new()),
    );

    let dotfile = seed_artifact_blob(&store, "ORB-00000", ".payload", b"dotfile", "codex");
    store
        .rewrite_artifact_manifest(
            "ORB-00000",
            &ArtifactManifestV2 {
                schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
                files: vec![dotfile],
            },
        )
        .expect("write artifact manifest");
    let bundle_dir = store.bundle_path("ORB-00000").expect("bundle path");
    fs::write(
        bundle_dir.join(crate::driver::file::task_bundle::PENDING_WRITE_FILE_NAME),
        "internal recovery state",
    )
    .expect("write pending sidecar");

    export_tasks(&registry, ws, ExportSelection::All, &archive, exported_at()).expect("export");

    let extracted = TempDir::new().unwrap();
    super::super::archive::extract_archive(&archive, extracted.path()).expect("extract");
    let archived_bundle = extracted.path().join("bundles/ORB-00000");
    assert!(
        !archived_bundle
            .join(crate::driver::file::task_bundle::PENDING_WRITE_FILE_NAME)
            .exists()
    );
    assert_eq!(
        fs::read(archived_bundle.join("artifacts/files/.payload")).expect("dotfile payload"),
        b"dotfile"
    );

    let target = open_registry(dst.path());
    import_tasks(&target, &archive, None, ImportConflictPolicy::Fail).expect("import");
    let imported = read_bundle_at(
        &target
            .canonical_task_bundle_path(ws, "ORB-00000")
            .expect("canonical path"),
    )
    .expect("dotfile artifact round trips");
    assert_eq!(imported.artifact_manifest.expect("manifest").files.len(), 1);
}

#[test]
fn export_racing_a_deletion_is_valid_or_reports_the_disappeared_bundle() {
    use std::sync::Barrier;
    use std::time::Duration;

    let src = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();
    let archive = src.path().join("tasks.tar.zst");
    let ws = "export-deleted-abcdef";
    let registry = open_registry(src.path());
    let binding = bind(&registry, src.path(), ws);
    let store = bundle_store(&registry, &binding);
    seed(
        &store,
        &registry,
        ws,
        &make_bundle("ORB-00000", "deleted", Vec::new()),
    );

    let bundle_dir = store.bundle_path("ORB-00000").expect("bundle path");
    let holding = Barrier::new(2);
    let release = Barrier::new(2);
    let (export_sent, export_received) = std::sync::mpsc::sync_channel(1);

    std::thread::scope(|scope| {
        scope.spawn(|| {
            store
                .with_bundle_write_lock("ORB-00000", || {
                    holding.wait();
                    release.wait();
                    Ok(())
                })
                .expect("hold bundle lock");
        });
        holding.wait();

        scope.spawn(|| {
            store
                .with_bundle_write_lock("ORB-00000", || {
                    fs::remove_dir_all(&bundle_dir).map_err(|error| {
                        orbit_common::OrbitError::Io(format!("delete bundle: {error}"))
                    })
                })
                .expect("delete bundle");
        });
        scope.spawn(|| {
            export_sent
                .send(export_tasks(
                    &registry,
                    ws,
                    ExportSelection::All,
                    &archive,
                    exported_at(),
                ))
                .expect("send export result");
        });
        release.wait();

        match export_received
            .recv_timeout(Duration::from_secs(10))
            .expect("export must finish")
        {
            Ok(outcome) => {
                assert_eq!(outcome.task_ids, vec!["ORB-00000"]);
                let target = open_registry(dst.path());
                let imported = import_tasks(&target, &archive, None, ImportConflictPolicy::Fail)
                    .expect("a completed export must import cleanly");
                assert_eq!(
                    imported.tasks.first().map(|task| task.final_id.as_str()),
                    Some("ORB-00000"),
                    "a completed export must retain the deleted task bundle"
                );
                let imported_bundle = target
                    .canonical_task_bundle_path(ws, "ORB-00000")
                    .expect("canonical imported bundle path");
                assert!(imported_bundle.is_dir());
                let imported_bundle = read_bundle_at(&imported_bundle)
                    .expect("a completed export must contain a readable task bundle");
                assert_eq!(imported_bundle.envelope.id, "ORB-00000");
            }
            Err(error) => {
                let message = error.to_string();
                let bundle_path = bundle_dir.display().to_string();
                assert!(
                    message.contains("missing") || message.contains("disappeared"),
                    "concurrent deletion must report the disappeared bundle: {message}"
                );
                assert!(
                    message.contains("ORB-00000"),
                    "concurrent deletion error must name the task ID: {message}"
                );
                assert!(
                    message.contains(&bundle_path),
                    "concurrent deletion error must name the bundle path: {message}"
                );
            }
        }
    });
}
