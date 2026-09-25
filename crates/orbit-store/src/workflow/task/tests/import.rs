use std::fs;
use std::path::PathBuf;

use orbit_types::task::{
    ArtifactManifestV2, TASK_ARTIFACT_SCHEMA_VERSION, TASK_ARTIFACTS_DIR_NAME,
};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

use crate::driver::file::task_bundle::read_bundle_at;

use super::*;

#[test]
fn round_trip_keeps_ids_and_content() {
    let src = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();
    let archive = src.path().join("tasks.tar.zst");
    let ws = "orbit-src-aaaaaa";
    let (a, b) = build_source_archive(src.path(), ws, &archive);

    // Target registry with the same workspace already registered but no tasks.
    let registry = open_registry(dst.path());
    let binding = bind(&registry, dst.path(), ws);

    let outcome =
        import_tasks(&registry, &archive, None, ImportConflictPolicy::Renumber).expect("import");
    assert_eq!(outcome.workspace_id, ws);
    assert!(!outcome.registered_workspace);
    assert!(outcome.id_remap.is_empty());
    assert!(outcome.id_map_path.is_none());
    assert_eq!(outcome.tasks.len(), 2);
    assert!(
        outcome
            .tasks
            .iter()
            .all(|task| task.action == ImportAction::Kept)
    );

    // Bundles landed byte-for-byte identical.
    let landed_a = read_bundle_at(
        &registry
            .canonical_task_bundle_path(ws, "ORB-00000")
            .unwrap(),
    )
    .unwrap();
    let landed_b = read_bundle_at(
        &registry
            .canonical_task_bundle_path(ws, "ORB-00001")
            .unwrap(),
    )
    .unwrap();
    assert_eq!(landed_a, a);
    assert_eq!(landed_b, b);
    assert_eq!(
        landed_a.envelope.orchestrator.as_deref(),
        Some("archive-orchestrator")
    );

    // Index rows exist and allocator advanced past ORB-00001.
    assert_eq!(
        registry.tasks_for_workspace(ws).unwrap().len(),
        2,
        "both tasks registered"
    );
    assert!(registry.allocator_next_number().unwrap() >= 2);

    assert!(!binding.orbit_dir.join("tasks").exists());
}

#[test]
fn collision_renumber_rewrites_relations() {
    let src = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();
    let archive = src.path().join("tasks.tar.zst");
    let src_ws = "orbit-src-aaaaaa";
    build_source_archive(src.path(), src_ws, &archive);

    // Target registry already owns ORB-00000 (different content) in another ws.
    let registry = open_registry(dst.path());
    let target_ws = "orbit-dst-bbbbbb";
    let binding = bind(&registry, dst.path(), target_ws);
    let store = bundle_store(&registry, &binding);
    seed(
        &store,
        &registry,
        target_ws,
        &make_bundle("ORB-00000", "pre-existing local task", Vec::new()),
    );

    let outcome = import_tasks(
        &registry,
        &archive,
        Some(target_ws),
        ImportConflictPolicy::Renumber,
    )
    .expect("import");

    // ORB-00000 collided and was renumbered; ORB-00001 stayed (free) but its
    // ChildOf target was rewritten to the new id.
    let new_id = outcome
        .id_remap
        .get("ORB-00000")
        .expect("ORB-00000 renumbered")
        .clone();
    assert_ne!(new_id, "ORB-00000");
    assert!(outcome.id_map_path.as_ref().is_some_and(|p| p.is_file()));

    let child = read_bundle_at(
        &registry
            .canonical_task_bundle_path(target_ws, "ORB-00001")
            .unwrap(),
    )
    .unwrap();
    assert_eq!(child.envelope.relations.len(), 1);
    assert_eq!(child.envelope.relations[0].target, new_id);

    // The renumbered task's envelope id matches its directory / new id.
    let renumbered = read_bundle_at(
        &registry
            .canonical_task_bundle_path(target_ws, &new_id)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(renumbered.envelope.id, new_id);

    // Original local ORB-00000 is untouched.
    let original = read_bundle_at(
        &registry
            .canonical_task_bundle_path(target_ws, "ORB-00000")
            .unwrap(),
    )
    .unwrap();
    assert_eq!(original.envelope.title, "pre-existing local task");
}

#[test]
fn import_into_unregistered_workspace_registers_it() {
    let src = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();
    let archive = src.path().join("tasks.tar.zst");
    let ws = "orbit-src-aaaaaa";
    build_source_archive(src.path(), ws, &archive);

    // Fresh target registry with nothing bound.
    let registry = open_registry(dst.path());
    assert!(registry.find_workspace_binding(ws).unwrap().is_none());

    let outcome =
        import_tasks(&registry, &archive, None, ImportConflictPolicy::Fail).expect("import");
    assert_eq!(outcome.workspace_id, ws);
    assert!(outcome.registered_workspace);
    assert!(registry.find_workspace_binding(ws).unwrap().is_some());
    assert!(
        registry.find_workspace_checkout(ws).unwrap().is_none(),
        "migration must not fabricate a detached checkout"
    );
    assert_eq!(registry.tasks_for_workspace(ws).unwrap().len(), 2);
    assert!(!dst.path().join("tasks/detached").exists());
}

#[test]
fn failed_import_removes_workspace_registered_by_that_run() {
    let src = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();
    let archive = src.path().join("tasks.tar.zst");
    let ws = "orbit-src-bbbbbb";
    build_source_archive(src.path(), ws, &archive);

    let registry = open_registry(dst.path());
    let binding = bind(&registry, dst.path(), ws);
    let store = bundle_store(&registry, &binding);
    seed(
        &store,
        &registry,
        ws,
        &make_bundle("ORB-00000", "orphaned bundle", Vec::new()),
    );
    registry
        .unbind_workspace(ws)
        .expect("remove the index while retaining the canonical bundle");
    assert!(registry.find_workspace_binding(ws).unwrap().is_none());

    let error = import_tasks(&registry, &archive, None, ImportConflictPolicy::Fail)
        .expect_err("creation-only import must reject the orphaned directory");
    assert!(error.to_string().contains("task bundle already exists"));
    assert!(
        registry.find_workspace_binding(ws).unwrap().is_none(),
        "a failed import must not leave its newly registered workspace behind"
    );
    assert!(
        registry
            .canonical_task_bundle_path(ws, "ORB-00000")
            .unwrap()
            .is_dir(),
        "rollback must preserve the pre-existing orphaned bundle"
    );
}

#[test]
fn allocator_bumped_past_max_imported_id() {
    let src = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();
    let archive = src.path().join("tasks.tar.zst");
    let ws = "orbit-hi-cccccc";

    // Source with a single high id ORB-00042.
    let registry = open_registry(src.path());
    let binding = bind(&registry, src.path(), ws);
    let store = bundle_store(&registry, &binding);
    seed(
        &store,
        &registry,
        ws,
        &make_bundle("ORB-00042", "high id", Vec::new()),
    );
    export_tasks(&registry, ws, ExportSelection::All, &archive, exported_at()).unwrap();

    let target = open_registry(dst.path());
    import_tasks(&target, &archive, None, ImportConflictPolicy::Fail).unwrap();
    assert_eq!(target.allocator_next_number().unwrap(), 43);
}

#[test]
fn reimport_is_idempotent() {
    let src = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();
    let archive = src.path().join("tasks.tar.zst");
    let ws = "orbit-src-aaaaaa";
    build_source_archive(src.path(), ws, &archive);

    let registry = open_registry(dst.path());
    bind(&registry, dst.path(), ws);
    import_tasks(&registry, &archive, None, ImportConflictPolicy::Renumber).unwrap();

    let second = import_tasks(&registry, &archive, None, ImportConflictPolicy::Renumber).unwrap();
    assert!(second.id_remap.is_empty());
    assert!(
        second
            .tasks
            .iter()
            .all(|task| task.action == ImportAction::AlreadyPresent)
    );
    assert_eq!(registry.tasks_for_workspace(ws).unwrap().len(), 2);
}

#[test]
fn on_conflict_skip_leaves_local_and_drops_incoming() {
    let src = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();
    let archive = src.path().join("tasks.tar.zst");
    let src_ws = "orbit-src-aaaaaa";
    build_source_archive(src.path(), src_ws, &archive);

    let registry = open_registry(dst.path());
    let target_ws = "orbit-dst-bbbbbb";
    let binding = bind(&registry, dst.path(), target_ws);
    let store = bundle_store(&registry, &binding);
    seed(
        &store,
        &registry,
        target_ws,
        &make_bundle("ORB-00000", "local keep", Vec::new()),
    );

    let outcome = import_tasks(
        &registry,
        &archive,
        Some(target_ws),
        ImportConflictPolicy::Skip,
    )
    .unwrap();
    // ORB-00000 skipped, ORB-00001 kept.
    assert!(outcome.id_remap.is_empty());
    let skipped = outcome
        .tasks
        .iter()
        .find(|t| t.source_id == "ORB-00000")
        .unwrap();
    assert_eq!(skipped.action, ImportAction::SkippedConflict);
    // local task unchanged
    let local = read_bundle_at(
        &registry
            .canonical_task_bundle_path(target_ws, "ORB-00000")
            .unwrap(),
    )
    .unwrap();
    assert_eq!(local.envelope.title, "local keep");
}

#[test]
fn on_conflict_fail_aborts_without_writing() {
    let src = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();
    let archive = src.path().join("tasks.tar.zst");
    let src_ws = "orbit-src-aaaaaa";
    build_source_archive(src.path(), src_ws, &archive);

    let registry = open_registry(dst.path());
    let target_ws = "orbit-dst-bbbbbb";
    let binding = bind(&registry, dst.path(), target_ws);
    let store = bundle_store(&registry, &binding);
    seed(
        &store,
        &registry,
        target_ws,
        &make_bundle("ORB-00000", "local keep", Vec::new()),
    );

    let err = import_tasks(
        &registry,
        &archive,
        Some(target_ws),
        ImportConflictPolicy::Fail,
    )
    .unwrap_err();
    assert!(format!("{err}").contains("already exists"));
    // Nothing new landed: only the pre-existing local task remains.
    assert_eq!(registry.tasks_for_workspace(target_ws).unwrap().len(), 1);
    assert!(
        !registry
            .canonical_task_bundle_path(target_ws, "ORB-00001")
            .unwrap()
            .exists()
    );
}

#[test]
fn corrupt_archive_fails_before_mutation() {
    let dst = TempDir::new().unwrap();
    let bogus = dst.path().join("bogus.tar.zst");
    fs::write(&bogus, b"not a real zstd archive").unwrap();
    let registry = open_registry(dst.path());
    let err = import_tasks(&registry, &bogus, None, ImportConflictPolicy::Renumber).unwrap_err();
    assert!(format!("{err}").contains("zstd") || format!("{err}").contains("archive"));
}

fn read_id_map(path: &PathBuf) -> std::collections::BTreeMap<String, String> {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

#[test]
fn renumber_writes_id_map_file() {
    let src = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();
    let archive = src.path().join("tasks.tar.zst");
    build_source_archive(src.path(), "orbit-src-aaaaaa", &archive);

    let registry = open_registry(dst.path());
    let target_ws = "orbit-dst-bbbbbb";
    let binding = bind(&registry, dst.path(), target_ws);
    let store = bundle_store(&registry, &binding);
    seed(
        &store,
        &registry,
        target_ws,
        &make_bundle("ORB-00000", "collide", Vec::new()),
    );

    let outcome = import_tasks(
        &registry,
        &archive,
        Some(target_ws),
        ImportConflictPolicy::Renumber,
    )
    .unwrap();
    let map_path = outcome.id_map_path.expect("id map written");
    let map = read_id_map(&map_path);
    assert_eq!(map.get("ORB-00000"), outcome.id_remap.get("ORB-00000"));
}

/// Regression for ORB-10042: import must round-trip artifact blobs. Before the
/// fix, `write_bundle_at` wrote only the manifest, so the canonical bundle
/// landed with `artifacts/files/` empty and `read_bundle_at` failed with
/// "artifact manifest references missing file". The test covers a nested blob
/// path (`artifacts/files/nested/output.bin`) to catch mkdir-parent regressions
/// in the copy step.
#[test]
fn round_trip_preserves_artifact_blobs() {
    let src = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();
    let archive = src.path().join("tasks.tar.zst");
    let ws = "orbit-art-eeeeee";

    // Source: one task with two blobs — a top-level file and a nested one.
    let registry = open_registry(src.path());
    let binding = bind(&registry, src.path(), ws);
    let store = bundle_store(&registry, &binding);
    let bundle = make_bundle("ORB-00000", "artifact task", Vec::new());
    seed(&store, &registry, ws, &bundle);
    let flat = seed_artifact_blob(&store, "ORB-00000", "top.txt", b"top-level", "codex");
    let nested = seed_artifact_blob(
        &store,
        "ORB-00000",
        "nested/output.bin",
        b"\x00\x01\x02deep",
        "codex",
    );
    let manifest = ArtifactManifestV2 {
        schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
        files: vec![flat.clone(), nested.clone()],
    };
    store
        .rewrite_artifact_manifest("ORB-00000", &manifest)
        .expect("rewrite manifest");
    // Reindex to reflect the new manifest state.
    registry
        .replace_task_index(ws, &store.read_bundle("ORB-00000").unwrap().envelope)
        .expect("reindex");
    // Sanity: the seeded bundle re-reads clean (validates blob hashes).
    let seeded = store.read_bundle("ORB-00000").expect("read seeded");
    assert_eq!(
        seeded.artifact_manifest.as_ref().map(|m| m.files.len()),
        Some(2)
    );

    export_tasks(&registry, ws, ExportSelection::All, &archive, exported_at()).unwrap();

    // Import into a fresh registry and verify the canonical bundle validates
    // end-to-end, including hash checks against restored blobs.
    let target_registry = open_registry(dst.path());
    let outcome =
        import_tasks(&target_registry, &archive, None, ImportConflictPolicy::Fail).expect("import");
    assert_eq!(outcome.tasks.len(), 1);
    assert_eq!(outcome.tasks[0].action, ImportAction::Kept);

    let landed_dir = target_registry
        .canonical_task_bundle_path(ws, "ORB-00000")
        .unwrap();
    // `read_bundle_at` re-hashes every blob file — surviving this is the
    // acceptance criterion for the round trip.
    let landed = read_bundle_at(&landed_dir).expect("read landed bundle");
    let landed_manifest = landed
        .artifact_manifest
        .expect("landed bundle has manifest");
    assert_eq!(landed_manifest.files.len(), 2);

    // Files present at the expected paths with the recorded hashes.
    for expected in [&flat, &nested] {
        let blob_path = landed_dir
            .join(TASK_ARTIFACTS_DIR_NAME)
            .join(&expected.blob);
        let bytes = fs::read(&blob_path).expect("blob present");
        assert_eq!(bytes.len() as u64, expected.size_bytes);
        assert_eq!(format!("{:x}", Sha256::digest(&bytes)), expected.sha256);
    }
}

#[test]
fn import_rejects_tampered_artifact_bytes() {
    let src = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();
    let archive = src.path().join("tasks.tar.zst");
    let ws = "ws_art_tamper";
    let registry = open_registry(src.path());
    let binding = bind(&registry, src.path(), ws);
    let store = bundle_store(&registry, &binding);
    seed(
        &store,
        &registry,
        ws,
        &make_bundle("ORB-00000", "artifact task", Vec::new()),
    );
    let entry = seed_artifact_blob(&store, "ORB-00000", "top.txt", b"original", "codex");
    store
        .rewrite_artifact_manifest(
            "ORB-00000",
            &ArtifactManifestV2 {
                schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
                files: vec![entry.clone()],
            },
        )
        .expect("rewrite manifest");
    fs::write(
        store
            .bundle_path("ORB-00000")
            .unwrap()
            .join(TASK_ARTIFACTS_DIR_NAME)
            .join(&entry.blob),
        b"xxxxxxxx",
    )
    .unwrap();
    export_tasks(&registry, ws, ExportSelection::All, &archive, exported_at()).unwrap();

    let target_registry = open_registry(dst.path());
    let error = import_tasks(&target_registry, &archive, None, ImportConflictPolicy::Fail)
        .unwrap_err()
        .to_string();
    assert!(error.contains("ORB-00000"), "{error}");
    assert!(error.contains("sha256 mismatch"), "{error}");
    assert!(target_registry.tasks_for_workspace(ws).unwrap().is_empty());
}

/// Regression for the pre-ORB-10042 "half-imported" state: a canonical bundle
/// with a manifest but no blob files. The documented backfill flow is to copy
/// the blob tree back from the retained archive. `copy_artifact_blobs` is the
/// primitive that path uses; this test pins its behavior so the recipe in the
/// module docs stays honest.
#[test]
fn backfill_via_copy_artifact_blobs_restores_stranded_bundle() {
    use crate::driver::file::task_bundle::bundle_io::copy_artifact_blobs;

    let src = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();
    let archive = src.path().join("tasks.tar.zst");
    let ws = "orbit-bkf-ffffff";

    let src_registry = open_registry(src.path());
    let src_binding = bind(&src_registry, src.path(), ws);
    let src_store = bundle_store(&src_registry, &src_binding);
    let seed_bundle = make_bundle("ORB-00007", "done task", Vec::new());
    seed(&src_store, &src_registry, ws, &seed_bundle);
    let entry = seed_artifact_blob(&src_store, "ORB-00007", "log.txt", b"payload", "codex");
    let manifest = ArtifactManifestV2 {
        schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
        files: vec![entry.clone()],
    };
    src_store
        .rewrite_artifact_manifest("ORB-00007", &manifest)
        .unwrap();
    src_registry
        .replace_task_index(ws, &src_store.read_bundle("ORB-00007").unwrap().envelope)
        .unwrap();
    export_tasks(
        &src_registry,
        ws,
        ExportSelection::All,
        &archive,
        exported_at(),
    )
    .unwrap();

    // Simulate a pre-fix stranded bundle: manifest present, blob missing.
    let dst_registry = open_registry(dst.path());
    let dst_binding = bind(&dst_registry, dst.path(), ws);
    let dst_store = bundle_store(&dst_registry, &dst_binding);
    seed(&dst_store, &dst_registry, ws, &seed_bundle);
    dst_store
        .rewrite_artifact_manifest("ORB-00007", &manifest)
        .unwrap();
    // Confirm read fails — this is the "stranded" state.
    assert!(dst_store.read_bundle("ORB-00007").is_err());

    // Extract the archive to a staging tree, mirroring the documented recipe.
    let staging = TempDir::new().unwrap();
    super::super::archive::extract_archive(&archive, staging.path()).unwrap();
    let staged_bundle_dir = staging.path().join("bundles").join("ORB-00007");
    let landed_dir = dst_registry
        .canonical_task_bundle_path(ws, "ORB-00007")
        .unwrap();
    copy_artifact_blobs(&staged_bundle_dir, &landed_dir, &manifest).expect("backfill blobs");

    // Bundle is now whole and re-reads cleanly.
    let restored = dst_store.read_bundle("ORB-00007").expect("read restored");
    let restored_manifest = restored.artifact_manifest.unwrap();
    assert_eq!(restored_manifest.files.len(), 1);
    let blob_path = landed_dir.join(TASK_ARTIFACTS_DIR_NAME).join(&entry.blob);
    assert_eq!(fs::read(&blob_path).unwrap(), b"payload");
}

#[test]
fn renumber_reimport_is_not_idempotent() {
    // Documented boundary: idempotency is scoped to kept (free) ids. A collision
    // under --on-conflict=renumber means "these are new local tasks", so each
    // re-run mints fresh ids rather than de-duplicating. This test pins that.
    let src = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();
    let archive = src.path().join("tasks.tar.zst");
    build_source_archive(src.path(), "orbit-src-aaaaaa", &archive);

    let registry = open_registry(dst.path());
    let target_ws = "orbit-dst-bbbbbb";
    let binding = bind(&registry, dst.path(), target_ws);
    let store = bundle_store(&registry, &binding);
    seed(
        &store,
        &registry,
        target_ws,
        &make_bundle("ORB-00000", "local keep", Vec::new()),
    );

    let first = import_tasks(
        &registry,
        &archive,
        Some(target_ws),
        ImportConflictPolicy::Renumber,
    )
    .unwrap();
    assert!(!first.id_remap.is_empty());
    let after_first = registry.tasks_for_workspace(target_ws).unwrap().len();

    let second = import_tasks(
        &registry,
        &archive,
        Some(target_ws),
        ImportConflictPolicy::Renumber,
    )
    .unwrap();
    // Re-run renumbers again rather than skipping — task count strictly grows.
    assert!(!second.id_remap.is_empty());
    let after_second = registry.tasks_for_workspace(target_ws).unwrap().len();
    assert!(
        after_second > after_first,
        "renumber re-import mints fresh copies (not idempotent): {after_first} -> {after_second}"
    );
}
