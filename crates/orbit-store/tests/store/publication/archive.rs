use std::fs;

use orbit_types::task::TASK_ARTIFACTS_DIR_NAME;
use tempfile::TempDir;

use super::support;
use orbit_store::compose::workspace_task_backends;
use orbit_store::contracts::TaskArtifactUpdateParams;
use orbit_store::maintenance::task_registry::{
    BindWorkspaceParams, TaskRegistryStore, task_registry_path,
};
use orbit_store::workflow::task::{
    ExportSelection, ImportConflictPolicy, export_tasks, import_tasks,
};
use orbit_types::task::{TaskArtifact, TaskStatus};

/// An archive is untrusted input. A blob that is a link to a host file whose
/// bytes match the manifest must not be dereferenced and copied into the store.
#[cfg(unix)]
#[test]
fn import_refuses_an_archive_entry_that_is_a_symlink() {
    if !support::isolated("archive::import_refuses_an_archive_entry_that_is_a_symlink") {
        return;
    }
    let src = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();
    let archive = src.path().join("tasks.tar.zst");
    let linked = src.path().join("linked.tar.zst");
    let ws = "ws_art_symlink";
    let registry = open_registry(src.path());
    let binding = bind(&registry, src.path(), ws);
    let backends = workspace_task_backends(registry.clone(), binding.partition_id);
    let id = backends
        .task
        .create_task(support::create_params("artifact task", TaskStatus::Backlog))
        .unwrap()
        .id;
    let host_bytes = b"host file the archive author knows byte for byte";
    backends
        .artifact
        .upsert_task_artifacts(
            &id,
            TaskArtifactUpdateParams {
                origin: None,
                actor: "codex".into(),
                owner_run_id: None,
                writer: None,
                upsert_artifacts: vec![TaskArtifact {
                    path: "top.txt".into(),
                    content: host_bytes.to_vec(),
                    media_type: "application/octet-stream".into(),
                    created_by: Some("codex".into()),
                }],
            },
        )
        .unwrap();
    let entry = backends
        .artifact
        .get_task_artifact_manifest(&id)
        .unwrap()
        .unwrap()
        .remove(0);
    export_tasks(
        &registry,
        ws,
        ExportSelection::All,
        &archive,
        chrono::Utc::now(),
    )
    .unwrap();

    // Unpack the honest archive, swap the blob for a link to a host file with
    // identical bytes, and pack it again with the link preserved.
    let unpacked = src.path().join("unpacked");
    let decoder = zstd::stream::read::Decoder::new(fs::File::open(&archive).unwrap()).unwrap();
    tar::Archive::new(decoder).unpack(&unpacked).unwrap();
    let host_file = src.path().join("host-file.txt");
    fs::write(&host_file, host_bytes).unwrap();
    let blob = unpacked
        .join(format!("bundles/{id}"))
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

/// A GNU sparse entry declaring a logical size past the shipped unpacked
/// bound (256 MiB) must be refused by name at import, even though its stored
/// bytes are empty, and must land no task.
#[test]
fn import_refuses_sparse_archive_over_unpacked_bound() {
    if !support::isolated("archive::import_refuses_sparse_archive_over_unpacked_bound") {
        return;
    }
    let dst = TempDir::new().unwrap();
    let archive_path = dst.path().join("sparse-bomb.tar.zst");
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::GNUSparse);
    header.set_path("bomb").unwrap();
    header.set_size(0);
    header
        .as_gnu_mut()
        .unwrap()
        .set_real_size(256 * 1024 * 1024 + 1);
    let gnu = header.as_gnu_mut().unwrap();
    gnu.sparse[0].set_offset(256 * 1024 * 1024 + 1);
    gnu.sparse[0].set_length(0);
    header.set_mode(0o644);
    header.set_cksum();
    let mut builder = tar::Builder::new(Vec::new());
    builder.append(&header, std::io::empty()).unwrap();
    let tar_bytes = builder.into_inner().unwrap();
    let encoder =
        zstd::stream::write::Encoder::new(fs::File::create(&archive_path).unwrap(), 3).unwrap();
    let mut encoder = encoder;
    {
        use std::io::Write as _;
        encoder.write_all(&tar_bytes).unwrap();
    }
    encoder.finish().unwrap();

    let target_registry = open_registry(dst.path());
    let error = import_tasks(
        &target_registry,
        &archive_path,
        None,
        ImportConflictPolicy::Fail,
    )
    .expect_err("a sparse entry over the unpacked bound must be refused");
    let error = error.to_string();
    assert!(
        error.contains("MAX_TASK_ARCHIVE_UNPACKED_BYTES"),
        "the refusal must name the unpacked bound: {error}"
    );
}

/// A regular entry declaring more than the shipped unpacked bound is
/// refused by name before its (here absent) payload is written.
#[test]
fn import_refuses_archive_over_unpacked_bound() {
    if !support::isolated("archive::import_refuses_archive_over_unpacked_bound") {
        return;
    }
    let dst = TempDir::new().unwrap();
    let archive_path = dst.path().join("bomb.tar.zst");
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Regular);
    header.set_path("bomb").unwrap();
    header.set_size(256 * 1024 * 1024 + 1);
    header.set_mode(0o644);
    header.set_cksum();
    let encoder =
        zstd::stream::write::Encoder::new(fs::File::create(&archive_path).unwrap(), 3).unwrap();
    let mut encoder = encoder;
    {
        use std::io::Write as _;
        encoder.write_all(header.as_bytes()).unwrap();
    }
    encoder.finish().unwrap();

    let target_registry = open_registry(dst.path());
    let error = import_tasks(
        &target_registry,
        &archive_path,
        None,
        ImportConflictPolicy::Fail,
    )
    .expect_err("an archive over the unpacked bound must be refused");
    let error = error.to_string();
    assert!(
        error.contains("MAX_TASK_ARCHIVE_UNPACKED_BYTES"),
        "the refusal must name the unpacked bound: {error}"
    );
}

/// A member escaping the staging directory is refused explicitly, not
/// silently skipped by `unpack_in`.
#[test]
fn import_refuses_member_escaping_destination() {
    if !support::isolated("archive::import_refuses_member_escaping_destination") {
        return;
    }
    let dst = TempDir::new().unwrap();
    let archive_path = dst.path().join("escape.tar.zst");
    let mut builder = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_size(5);
    header.set_mode(0o644);
    header.set_entry_type(tar::EntryType::Regular);
    let raw = header.as_mut_bytes();
    raw[..14].copy_from_slice(b"../escaped.txt");
    header.set_cksum();
    builder.append(&header, &b"owned"[..]).unwrap();
    let tar_bytes = builder.into_inner().unwrap();
    let encoder =
        zstd::stream::write::Encoder::new(fs::File::create(&archive_path).unwrap(), 3).unwrap();
    let mut encoder = encoder;
    {
        use std::io::Write as _;
        encoder.write_all(&tar_bytes).unwrap();
    }
    encoder.finish().unwrap();

    let target_registry = open_registry(dst.path());
    let error = import_tasks(
        &target_registry,
        &archive_path,
        None,
        ImportConflictPolicy::Fail,
    )
    .expect_err("a traversing member must be refused explicitly");
    let error = error.to_string();
    assert!(
        error.contains("escaped.txt") && error.contains(".."),
        "the refusal must name the entry and why: {error}"
    );
}

/// More entries than the shipped entry bound (20,000) is refused by name.
#[test]
fn import_refuses_archive_over_entry_bound() {
    if !support::isolated("archive::import_refuses_archive_over_entry_bound") {
        return;
    }
    let dst = TempDir::new().unwrap();
    let archive_path = dst.path().join("many.tar.zst");
    let encoder =
        zstd::stream::write::Encoder::new(fs::File::create(&archive_path).unwrap(), 3).unwrap();
    let mut builder = tar::Builder::new(encoder);
    let mut header = tar::Header::new_gnu();
    header.set_size(0);
    header.set_mode(0o644);
    header.set_entry_type(tar::EntryType::Regular);
    header.set_path("a").unwrap();
    header.set_cksum();
    for _ in 0..20_001 {
        builder.append(&header, std::io::empty()).unwrap();
    }
    builder.into_inner().unwrap().finish().unwrap();

    let target_registry = open_registry(dst.path());
    let error = import_tasks(
        &target_registry,
        &archive_path,
        None,
        ImportConflictPolicy::Fail,
    )
    .expect_err("an archive over the entry bound must be refused");
    let error = error.to_string();
    assert!(
        error.contains("MAX_TASK_ARCHIVE_ENTRIES"),
        "the refusal must name the entry bound: {error}"
    );
}

fn open_registry(root: &std::path::Path) -> TaskRegistryStore {
    TaskRegistryStore::open(&task_registry_path(root)).unwrap()
}
fn bind(
    registry: &TaskRegistryStore,
    root: &std::path::Path,
    ws: &str,
) -> orbit_store::maintenance::task_registry::WorkspaceCheckoutBinding {
    let repo = root.join("repo");
    let orbit_dir = repo.join(".orbit");
    fs::create_dir_all(&orbit_dir).unwrap();
    registry
        .bind_workspace(BindWorkspaceParams {
            partition_id: Some(ws.into()),
            slug: "sample".into(),
            repo_root: repo.clone(),
            workspace_path: repo,
            orbit_dir,
            repo_fingerprint: None,
        })
        .unwrap()
}
