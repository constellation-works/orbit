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

mod export;
mod import;
mod inspect;
mod owner_wins;
mod publication;
mod publish;
mod reindex;
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
            partition_id: Some(ws_id.to_string()),
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
    TaskBundleStoreV2::new(registry.clone(), binding.partition_id.clone())
}

fn make_bundle(id: &str, title: &str, relations: Vec<TaskRelation>) -> TaskBundleV2 {
    let now = Utc.with_ymd_and_hms(2026, 6, 1, 9, 0, 0).unwrap();
    TaskBundleV2 {
        envelope: TaskEnvelopeV2 {
            job_run_machine: None,
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
        origin: None,
        path: path.to_string(),
        blob,
        sha256: format!("{:x}", Sha256::digest(bytes)),
        media_type: "application/octet-stream".to_string(),
        size_bytes: bytes.len() as u64,
        created_by: actor.to_string(),
        created_at: Utc.with_ymd_and_hms(2026, 6, 1, 9, 0, 0).unwrap(),
    }
}
