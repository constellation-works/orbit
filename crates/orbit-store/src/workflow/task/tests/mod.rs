use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use chrono::{TimeZone, Utc};
use orbit_types::task::{
    ArtifactManifestFileV2, ArtifactManifestV2, TASK_ARTIFACT_FILES_DIR_NAME,
    TASK_ARTIFACT_SCHEMA_VERSION, TASK_ARTIFACTS_DIR_NAME, TaskEnvelopeV2, TaskEventRowV2,
    TaskPriority, TaskRelation, TaskRelationType, TaskStatus, TaskType,
};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

use crate::driver::file::task_bundle::{TaskBundleV2, read_bundle_at};
use crate::driver::sqlite::task_registry::{
    BindWorkspaceParams, TaskRegistryStore, WorkspaceCheckoutBinding, task_registry_path,
};
use crate::repository::task::v2_bundle::TaskBundleStoreV2;

use super::*;

mod inspect;
mod owner_wins;
mod publication;
mod publish;
mod restore;

/// Run `git` with a deterministic, non-interactive identity. Publication tests
/// drive only local temporary repositories: there is no network service and no
/// ambient credential.
fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(["-c", "core.hooksPath=/dev/null"])
        .args(args)
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "orbit-test")
        .env("GIT_AUTHOR_EMAIL", "orbit-test@example.test")
        .env("GIT_COMMITTER_NAME", "orbit-test")
        .env("GIT_COMMITTER_EMAIL", "orbit-test@example.test")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// Raw Git stdout, including trailing CR/LF. `git()` trims and must not be
/// used to assert snapshot attachment bytes.
fn git_binary(dir: &Path, args: &[&str]) -> Vec<u8> {
    let output = Command::new("git")
        .args(["-c", "core.hooksPath=/dev/null"])
        .args(args)
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "orbit-test")
        .env("GIT_AUTHOR_EMAIL", "orbit-test@example.test")
        .env("GIT_COMMITTER_NAME", "orbit-test")
        .env("GIT_COMMITTER_EMAIL", "orbit-test@example.test")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

/// Six-byte CRLF payload used to detect Git text conversion (`61 0d 0a 62 0d 0a`).
const CRLF_PAYLOAD: &[u8] = b"a\r\nb\r\n";
const GITATTRIBUTES_CRLF: &[u8] = b"payload.txt text filter=orbit-sentinel\n";

fn include_attachment_policy() -> AttachmentPolicy {
    AttachmentPolicy {
        kind: AttachmentPolicyKind::Include,
        max_file_bytes: 1024,
        max_total_bytes: 4096,
        deny_patterns: Vec::new(),
        scanner_failure_behavior: ScannerFailureBehavior::AllowUnchecked,
    }
}

fn seed_crlf_gitattributes_attachments(
    store: &TaskBundleStoreV2,
    task_id: &str,
) -> ArtifactManifestV2 {
    let attrs = seed_artifact_blob(
        store,
        task_id,
        ".gitattributes",
        GITATTRIBUTES_CRLF,
        "codex",
    );
    let payload = seed_artifact_blob(store, task_id, "payload.txt", CRLF_PAYLOAD, "codex");
    let manifest = ArtifactManifestV2 {
        schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
        files: vec![attrs, payload],
    };
    store
        .rewrite_artifact_manifest(task_id, &manifest)
        .expect("rewrite crlf artifact manifest");
    manifest
}

fn git_add_literal(dir: &Path) {
    for (rel, _) in tree_bytes(dir) {
        let oid = git(dir, &["hash-object", "-w", "--no-filters", "--", &rel]);
        let cacheinfo = format!("100644,{oid},{rel}");
        git(dir, &["update-index", "--add", "--cacheinfo", &cacheinfo]);
    }
}

fn write_sentinel_gitconfig(dir: &Path) -> (PathBuf, PathBuf) {
    let sentinel = dir.join("orbit-sentinel-ran");
    let config = dir.join("poisoned.gitconfig");
    let sentinel_path = sentinel.to_str().expect("utf-8 sentinel path");
    fs::write(
        &config,
        format!(
            "[filter \"orbit-sentinel\"]\n\
             \tclean = sh -c \"echo ran >> '{sentinel_path}'; cat\"\n\
             \tsmudge = sh -c \"echo ran >> '{sentinel_path}'; cat\"\n\
             \trequired = true\n"
        ),
    )
    .expect("write poisoned gitconfig");
    (config, sentinel)
}

fn poison_publication_git_filters(dir: &Path) -> (orbit_common::test_env::ScopedEnv, PathBuf) {
    let (config, sentinel) = write_sentinel_gitconfig(dir);
    let config_path = config.to_str().expect("utf-8 gitconfig path").to_string();
    let env = orbit_common::test_env::scoped([("GIT_CONFIG_GLOBAL", Some(config_path.as_str()))]);
    (env, sentinel)
}

fn copy_tree(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let dest = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &dest);
        } else {
            fs::copy(entry.path(), dest).unwrap();
        }
    }
}

fn replace_worktree(repo: &Path, snapshot: &Path) {
    for entry in fs::read_dir(repo).unwrap() {
        let entry = entry.unwrap();
        if entry.file_name() == ".git" {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            fs::remove_dir_all(&path).unwrap();
        } else {
            fs::remove_file(&path).unwrap();
        }
    }
    copy_tree(snapshot, repo);
}

fn tree_bytes(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn visit(root: &Path, path: &Path, output: &mut BTreeMap<String, Vec<u8>>) {
        let mut entries: Vec<_> = fs::read_dir(path)
            .unwrap()
            .map(|entry| entry.unwrap())
            .collect();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            if entry.file_name() == ".git" {
                continue;
            }
            let path = entry.path();
            let file_type = entry.file_type().unwrap();
            if file_type.is_dir() || (file_type.is_symlink() && path.is_dir()) {
                visit(root, &path, output);
            } else if file_type.is_file() {
                output.insert(
                    path.strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/"),
                    fs::read(path).unwrap(),
                );
            }
        }
    }
    let mut output = BTreeMap::new();
    visit(root, root, &mut output);
    output
}

fn open_registry(global: &Path) -> TaskRegistryStore {
    TaskRegistryStore::open(&task_registry_path(global)).expect("open registry")
}

fn bind(registry: &TaskRegistryStore, global: &Path, ws_id: &str) -> WorkspaceCheckoutBinding {
    let orbit_dir = global.join("repos").join(ws_id).join(".orbit");
    fs::create_dir_all(&orbit_dir).expect("create orbit dir");
    registry
        .bind_workspace(BindWorkspaceParams {
            workspace_id: Some(ws_id.to_string()),
            slug: "sample".to_string(),
            repo_root: orbit_dir.parent().unwrap().to_path_buf(),
            workspace_path: orbit_dir.parent().unwrap().to_path_buf(),
            orbit_dir,
            repo_fingerprint: None,
        })
        .expect("bind workspace")
}

fn bundle_store(
    registry: &TaskRegistryStore,
    binding: &WorkspaceCheckoutBinding,
) -> TaskBundleStoreV2 {
    TaskBundleStoreV2::new(registry.clone(), binding.workspace_id.clone())
}

fn make_bundle(id: &str, title: &str, relations: Vec<TaskRelation>) -> TaskBundleV2 {
    let now = Utc.with_ymd_and_hms(2026, 6, 1, 9, 0, 0).unwrap();
    TaskBundleV2 {
        envelope: TaskEnvelopeV2 {
            schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
            id: id.to_string(),
            title: title.to_string(),
            status: TaskStatus::Backlog,
            task_type: TaskType::Feature,
            priority: TaskPriority::High,
            complexity: None,
            pr_status: None,
            job_run_id: None,
            crew: None,
            orchestrator: Some("archive-orchestrator".to_string()),
            relations,
            tags: vec!["migration".to_string()],
            required_tools: Vec::new(),
            context_files: Vec::new(),
            external_refs: Vec::new(),
            created_by: Some("codex".to_string()),
            planned_by: None,
            implemented_by: None,
            created_at: now,
            updated_at: now,
        },
        description: format!("description for {id}"),
        acceptance: "- [ ] done".to_string(),
        plan: "plan".to_string(),
        execution_summary: String::new(),
        events: vec![TaskEventRowV2 {
            schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
            event_id: "EV-0001".to_string(),
            at: now,
            by: "codex".to_string(),
            event_type: "created".to_string(),
            note: None,
            from_status: None,
            to_status: Some(TaskStatus::Backlog),
        }],
        comments: Vec::new(),
        artifact_manifest: None,
    }
}

fn child_of(target: &str) -> TaskRelation {
    TaskRelation {
        relation_type: TaskRelationType::ChildOf,
        target: target.to_string(),
    }
}

/// Write a bundle to disk and register+index it (no allocator advance — ids are
/// chosen explicitly by the test).
fn seed(store: &TaskBundleStoreV2, registry: &TaskRegistryStore, ws: &str, bundle: &TaskBundleV2) {
    store.create_bundle(bundle).expect("create bundle");
    registry
        .replace_task_index(ws, &bundle.envelope)
        .expect("index bundle");
}

fn exported_at() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 7, 4, 0, 0, 0).unwrap()
}

/// Build a source registry with two related tasks and export it to `archive`.
/// Returns (source bundles for ORB-00000, ORB-00001).
fn build_source_archive(
    global: &Path,
    ws_id: &str,
    archive: &Path,
) -> (TaskBundleV2, TaskBundleV2) {
    let registry = open_registry(global);
    let binding = bind(&registry, global, ws_id);
    let store = bundle_store(&registry, &binding);
    let a = make_bundle("ORB-00000", "root task", Vec::new());
    // ORB-00001 is a child of ORB-00000 — its ChildOf target must be rewritten
    // if ORB-00000 is renumbered on import.
    let b = make_bundle("ORB-00001", "child task", vec![child_of("ORB-00000")]);
    seed(&store, &registry, ws_id, &a);
    seed(&store, &registry, ws_id, &b);
    let outcome = export_tasks(
        &registry,
        ws_id,
        ExportSelection::All,
        archive,
        exported_at(),
    )
    .expect("export");
    assert_eq!(outcome.task_ids, vec!["ORB-00000", "ORB-00001"]);
    assert!(archive.is_file());
    (a, b)
}

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
    super::archive::extract_archive(&archive, extracted.path()).expect("extract");
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

#[test]
fn corrupt_archive_fails_before_mutation() {
    let dst = TempDir::new().unwrap();
    let bogus = dst.path().join("bogus.tar.zst");
    fs::write(&bogus, b"not a real zstd archive").unwrap();
    let registry = open_registry(dst.path());
    let err = import_tasks(&registry, &bogus, None, ImportConflictPolicy::Renumber).unwrap_err();
    assert!(format!("{err}").contains("zstd") || format!("{err}").contains("archive"));
}

#[test]
fn reindex_reregisters_disk_bundles_and_drops_stale() {
    let temp = TempDir::new().unwrap();
    let ws = "orbit-idx-dddddd";
    let registry = open_registry(temp.path());
    let binding = bind(&registry, temp.path(), ws);
    let store = bundle_store(&registry, &binding);
    seed(
        &store,
        &registry,
        ws,
        &make_bundle("ORB-00000", "a", Vec::new()),
    );
    seed(
        &store,
        &registry,
        ws,
        &make_bundle("ORB-00003", "b", Vec::new()),
    );

    // Simulate drift: drop ORB-00003's index+binding (dir still on disk) and add
    // a stale binding for ORB-00009 whose dir does not exist.
    registry.unregister_task_bundle("ORB-00003", ws).unwrap();
    let stale_dir = registry
        .canonical_task_bundle_path(ws, "ORB-00009")
        .unwrap();
    fs::create_dir_all(&stale_dir).unwrap();
    registry
        .register_task_bundle("ORB-00009", ws, &stale_dir)
        .unwrap();
    fs::remove_dir_all(&stale_dir).unwrap();

    let outcome = reindex_workspace(&registry, ws).expect("reindex");
    assert_eq!(outcome.indexed, 2, "two on-disk bundles reindexed");
    assert_eq!(outcome.removed_stale, 1, "stale ORB-00009 dropped");

    let registered: Vec<String> = registry
        .tasks_for_workspace(ws)
        .unwrap()
        .into_iter()
        .map(|t| t.task_id)
        .collect();
    assert_eq!(registered, vec!["ORB-00000", "ORB-00003"]);
    // Allocator moved past the highest on-disk id.
    assert!(registry.allocator_next_number().unwrap() >= 4);

    let again = reindex_workspace(&registry, ws).expect("reindex again");
    assert_eq!(again.indexed, outcome.indexed);
    assert_eq!(again.removed_stale, 0);
    assert_eq!(registry.allocator_next_number().unwrap(), 4);
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

/// Seed a blob at `path` (relative to the bundle's `artifacts/files/` dir) and
/// return the manifest entry describing it. Callers must merge the returned
/// entries into a single `ArtifactManifestV2` and `rewrite_artifact_manifest`
/// so `read_bundle_at` accepts the bundle.
fn seed_artifact_blob(
    store: &TaskBundleStoreV2,
    task_id: &str,
    path: &str,
    bytes: &[u8],
    actor: &str,
) -> ArtifactManifestFileV2 {
    let bundle_dir = store.bundle_path(task_id).expect("bundle path");
    let blob = format!("{TASK_ARTIFACT_FILES_DIR_NAME}/{path}");
    let blob_path = bundle_dir.join(TASK_ARTIFACTS_DIR_NAME).join(&blob);
    if let Some(parent) = blob_path.parent() {
        fs::create_dir_all(parent).expect("create artifact parent");
    }
    fs::write(&blob_path, bytes).expect("write blob");
    ArtifactManifestFileV2 {
        path: path.to_string(),
        blob,
        sha256: format!("{:x}", Sha256::digest(bytes)),
        media_type: "application/octet-stream".to_string(),
        size_bytes: bytes.len() as u64,
        created_by: actor.to_string(),
        created_at: Utc.with_ymd_and_hms(2026, 6, 1, 9, 0, 0).unwrap(),
    }
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
    super::archive::extract_archive(&archive, staging.path()).unwrap();
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
