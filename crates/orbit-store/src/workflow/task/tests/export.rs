use std::fs;
use std::process::Command;

use chrono::Utc;
use orbit_types::task::{
    ArtifactManifestV2, TASK_ARTIFACT_SCHEMA_VERSION, TASK_ARTIFACTS_DIR_NAME,
    TASK_EVENTS_FILE_NAME, TaskEventRowV2, TaskStatus,
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
fn export_refuses_interrupted_status_and_document_writes() {
    for kind in ["status", "document"] {
        let output = Command::new(std::env::current_exe().expect("current test executable"))
            .arg("export_refuses_interrupted_write_child")
            .arg("--ignored")
            .env("ORBIT_EXPORT_INTERRUPTED_WRITE_KIND", kind)
            .output()
            .expect("run interrupted-write child");
        assert!(
            output.status.success(),
            "{kind} child failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
#[ignore = "helper process for export_refuses_interrupted_status_and_document_writes"]
fn export_refuses_interrupted_write_child() {
    use crate::driver::file::task_bundle::{
        BundleWriteFault, PENDING_WRITE_FILE_NAME, PendingWriteGuard, TaskDocumentV2,
        append_jsonl_row, inject_bundle_write_faults,
    };

    let Ok(kind) = std::env::var("ORBIT_EXPORT_INTERRUPTED_WRITE_KIND") else {
        return;
    };

    let src = TempDir::new().expect("tempdir");
    let archive = src.path().join("tasks.tar.zst");
    let ws = "export-interrupted-abcdef";
    let registry = open_registry(src.path());
    let binding = bind(&registry, src.path(), ws);
    let store = bundle_store(&registry, &binding);
    seed(
        &store,
        &registry,
        ws,
        &make_bundle("ORB-00000", "interrupted", Vec::new()),
    );
    let bundle_dir = store.bundle_path("ORB-00000").expect("bundle path");
    let pending_path = bundle_dir.join(PENDING_WRITE_FILE_NAME);

    store
        .with_bundle_write_lock("ORB-00000", || {
            let pending = PendingWriteGuard::begin(&bundle_dir)?;
            match kind.as_str() {
                "status" => append_jsonl_row(
                    &bundle_dir.join(TASK_EVENTS_FILE_NAME),
                    &TaskEventRowV2 {
                        schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
                        event_id: "EV-0002".to_string(),
                        at: Utc::now(),
                        by: "codex".to_string(),
                        event_type: "status_changed".to_string(),
                        note: None,
                        from_status: Some(TaskStatus::Backlog),
                        to_status: Some(TaskStatus::InProgress),
                    },
                )?,
                "document" => store.rewrite_document(
                    "ORB-00000",
                    TaskDocumentV2::Description,
                    "uncommitted description",
                )?,
                other => panic!("unexpected interrupted-write kind: {other}"),
            }
            inject_bundle_write_faults(&[BundleWriteFault::DuringCompensation]);
            drop(pending);
            Ok(())
        })
        .expect("leave interrupted write state");
    assert!(
        pending_path.is_file(),
        "pending recovery record must remain"
    );

    let error = export_tasks(&registry, ws, ExportSelection::All, &archive, exported_at())
        .expect_err("export must refuse an interrupted write");
    let message = error.to_string();
    assert!(message.contains("pending-write"), "{message}");
    assert!(message.contains("ORB-00000"), "{message}");
    assert!(message.contains("recover or reindex"), "{message}");
    assert!(
        !archive.exists(),
        "refused export must not create an archive"
    );
    assert!(
        pending_path.is_file(),
        "export must preserve recovery evidence"
    );

    match kind.as_str() {
        "status" => {
            let events = fs::read_to_string(bundle_dir.join(TASK_EVENTS_FILE_NAME))
                .expect("read interrupted events");
            assert_eq!(
                events.lines().count(),
                2,
                "appended status event is retained"
            );
            let last: TaskEventRowV2 =
                serde_json::from_str(events.lines().last().expect("last event"))
                    .expect("parse appended event");
            assert_eq!(last.to_status, Some(TaskStatus::InProgress));
        }
        "document" => assert_eq!(
            fs::read_to_string(bundle_dir.join("description.md")).expect("read document"),
            "uncommitted description"
        ),
        _ => unreachable!(),
    }
}

#[test]
fn export_round_trips_pending_write_named_artifacts_and_excludes_root_sidecar() {
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

    let root_artifact = seed_artifact_blob(
        &store,
        "ORB-00000",
        ".pending-write.yaml",
        b"root artifact",
        "codex",
    );
    let nested_artifact = seed_artifact_blob(
        &store,
        "ORB-00000",
        "nested/.pending-write.yaml",
        b"nested artifact bytes\x00",
        "codex",
    );
    store
        .rewrite_artifact_manifest(
            "ORB-00000",
            &ArtifactManifestV2 {
                schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
                files: vec![root_artifact.clone(), nested_artifact.clone()],
            },
        )
        .expect("write artifact manifest");

    export_tasks(&registry, ws, ExportSelection::All, &archive, exported_at())
        .expect("export task with pending-write-named artifacts");
    let target_registry = open_registry(dst.path());
    let imported = import_tasks(&target_registry, &archive, None, ImportConflictPolicy::Fail)
        .expect("import task with pending-write-named artifacts");
    assert_eq!(imported.tasks.len(), 1);
    assert_eq!(imported.tasks[0].action, ImportAction::Kept);

    let landed_dir = target_registry
        .canonical_task_bundle_path(ws, "ORB-00000")
        .expect("landed bundle path");
    let landed = read_bundle_at(&landed_dir).expect("valid imported artifact manifest and blobs");
    let landed_manifest = landed
        .artifact_manifest
        .expect("imported bundle has artifact manifest");
    assert_eq!(
        landed_manifest.files,
        vec![root_artifact.clone(), nested_artifact.clone()]
    );
    for (artifact, expected_bytes) in [
        (&root_artifact, &b"root artifact"[..]),
        (&nested_artifact, &b"nested artifact bytes\x00"[..]),
    ] {
        let blob_path = landed_dir
            .join(TASK_ARTIFACTS_DIR_NAME)
            .join(&artifact.blob);
        assert_eq!(
            fs::read(blob_path).expect("imported artifact bytes"),
            expected_bytes
        );
    }

    // Normal export rejects recovery state during preflight. Exercise the
    // packer's defense-in-depth exclusion against an actual bundle-root file.
    let bundle_dir = store.bundle_path("ORB-00000").expect("bundle path");
    fs::write(
        bundle_dir.join(crate::driver::file::task_bundle::PENDING_WRITE_FILE_NAME),
        b"internal recovery state",
    )
    .expect("write pending sidecar");

    let sidecar_archive = src.path().join("sidecar.tar.zst");
    let file = fs::File::create(&sidecar_archive).expect("create sidecar archive");
    let encoder = zstd::stream::write::Encoder::new(file, 3).expect("zstd encoder");
    let mut builder = tar::Builder::new(encoder);
    builder.mode(tar::HeaderMode::Deterministic);
    super::super::archive::append_bundle_tree(&mut builder, "bundles/ORB-00000", &bundle_dir)
        .expect("pack bundle tree");
    builder
        .into_inner()
        .expect("finish tar")
        .finish()
        .expect("finish zstd");

    let extracted = TempDir::new().unwrap();
    super::super::archive::extract_archive(&sidecar_archive, extracted.path()).expect("extract");
    let archived_bundle = extracted.path().join("bundles/ORB-00000");
    assert!(
        !archived_bundle
            .join(crate::driver::file::task_bundle::PENDING_WRITE_FILE_NAME)
            .exists()
    );
    assert_eq!(
        fs::read(
            archived_bundle
                .join(TASK_ARTIFACTS_DIR_NAME)
                .join(&nested_artifact.blob)
        )
        .expect("nested pending-write artifact retained"),
        b"nested artifact bytes\x00"
    );
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
