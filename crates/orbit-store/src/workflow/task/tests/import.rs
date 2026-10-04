use std::fs;

use orbit_types::task::{
    ArtifactManifestV2, TASK_ARTIFACT_SCHEMA_VERSION, TASK_ARTIFACTS_DIR_NAME,
};
use tempfile::TempDir;

use super::*;

/// An archive is untrusted input. A blob that is a link to a host file whose
/// bytes match the manifest must not be dereferenced and copied into the store.
#[cfg(unix)]
#[test]
fn import_refuses_an_archive_entry_that_is_a_symlink() {
    let src = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();
    let archive = src.path().join("tasks.tar.zst");
    let linked = src.path().join("linked.tar.zst");
    let ws = "ws_art_symlink";
    let registry = open_registry(src.path());
    let binding = bind(&registry, src.path(), ws);
    let store = bundle_store(&registry, &binding);
    seed(
        &store,
        &registry,
        ws,
        &make_bundle("ORB-00000", "artifact task", Vec::new()),
    );
    let host_bytes = b"host file the archive author knows byte for byte";
    let entry = seed_artifact_blob(&store, "ORB-00000", "top.txt", host_bytes, "codex");
    store
        .rewrite_artifact_manifest(
            "ORB-00000",
            &ArtifactManifestV2 {
                schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
                files: vec![entry.clone()],
            },
        )
        .expect("rewrite manifest");
    export_tasks(&registry, ws, ExportSelection::All, &archive, exported_at()).unwrap();

    // Unpack the honest archive, swap the blob for a link to a host file with
    // identical bytes, and pack it again with the link preserved.
    let unpacked = src.path().join("unpacked");
    let decoder = zstd::stream::read::Decoder::new(fs::File::open(&archive).unwrap()).unwrap();
    tar::Archive::new(decoder).unpack(&unpacked).unwrap();
    let host_file = src.path().join("host-file.txt");
    fs::write(&host_file, host_bytes).unwrap();
    let blob = unpacked
        .join("bundles/ORB-00000")
        .join(TASK_ARTIFACTS_DIR_NAME)
        .join(&entry.blob);
    fs::remove_file(&blob).unwrap();
    std::os::unix::fs::symlink(&host_file, &blob).unwrap();
    let encoder = zstd::stream::write::Encoder::new(fs::File::create(&linked).unwrap(), 3).unwrap();
    let mut builder = tar::Builder::new(encoder);
    builder.follow_symlinks(false);
    builder.append_dir_all(".", &unpacked).unwrap();
    builder.into_inner().unwrap().finish().unwrap();

    let target_registry = open_registry(dst.path());
    let error = import_tasks(&target_registry, &linked, None, ImportConflictPolicy::Fail)
        .expect_err("an archive holding a symlink must be refused");
    assert!(error.to_string().contains("link"), "{error}");
    assert!(target_registry.tasks_for_workspace(ws).unwrap().is_empty());
}
