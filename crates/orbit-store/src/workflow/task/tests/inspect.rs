use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use orbit_types::task::{ArtifactManifestV2, TASK_ARTIFACT_SCHEMA_VERSION, TASK_EVENTS_FILE_NAME};
use tempfile::TempDir;

use super::*;
use crate::workflow::task::inspect::{
    SnapshotCheckpoint, load_validated_publication, set_snapshot_checkpoint_hook,
};

/// Upper bound for one side of a deterministic interleaving to reach or leave
/// its checkpoint; only a hung or deadlocked read gets near it.
const CHECKPOINT_WAIT: Duration = Duration::from_secs(60);

fn metadata(
    workspace_id: &str,
    generation: u64,
    previous: Option<&str>,
) -> PublicationSnapshotMetadata {
    PublicationSnapshotMetadata {
        publication_id: "pub_orbit_primary".to_string(),
        workspace_id: workspace_id.to_string(),
        source_repository_fingerprint: "git@github.com:example/orbit-source.git".to_string(),
        authority_machine_id: "hm_owner".to_string(),
        generation,
        published_at: Utc
            .with_ymd_and_hms(2026, 8, 30, 1, 2, generation as u32)
            .unwrap(),
        previous_publication: previous.map(ToOwned::to_owned),
    }
}

fn policy(kind: AttachmentPolicyKind) -> AttachmentPolicy {
    AttachmentPolicy {
        kind,
        max_file_bytes: 1024,
        max_total_bytes: 4096,
        deny_patterns: Vec::new(),
        scanner_failure_behavior: ScannerFailureBehavior::AllowUnchecked,
    }
}

fn request(
    workspace_id: &str,
    remote: &Path,
    cache: &Path,
    commit: Option<&str>,
) -> PublicationInspectRequest {
    PublicationInspectRequest {
        workspace_id: workspace_id.to_string(),
        source_repository_fingerprint: "git@github.com:example/orbit-source.git".to_string(),
        publication_id: "pub_orbit_primary".to_string(),
        authority_machine_id: "hm_owner".to_string(),
        publication_remote: remote.to_string_lossy().into_owned(),
        publication_branch: "refs/heads/main".to_string(),
        cache_dir: cache.to_path_buf(),
        commit: commit.map(ToOwned::to_owned),
    }
}

fn init_repo(dir: &Path) {
    fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-b", "main"]);
}

fn commit_snapshot(repo: &Path, snapshot: &Path, message: &str) -> String {
    replace_worktree(repo, snapshot);
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-m", message]);
    git(repo, &["rev-parse", "HEAD"])
}

fn amend_current(repo: &Path) {
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "--amend", "--no-edit"]);
}

fn seed_one(
    store: &TaskBundleStoreV2,
    registry: &TaskRegistryStore,
    workspace_id: &str,
    task_id: &str,
    files: &[(&str, &[u8])],
) {
    if files.is_empty() {
        seed(
            store,
            registry,
            workspace_id,
            &make_bundle(task_id, task_id, Vec::new()),
        );
        return;
    }
    seed(
        store,
        registry,
        workspace_id,
        &make_bundle(task_id, task_id, Vec::new()),
    );
    let entries: Vec<_> = files
        .iter()
        .map(|(path, bytes)| seed_artifact_blob(store, task_id, path, bytes, "codex"))
        .collect();
    store
        .rewrite_artifact_manifest(
            task_id,
            &ArtifactManifestV2 {
                schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
                files: entries,
            },
        )
        .unwrap();
}

struct LinearRepo {
    root: TempDir,
    remote: PathBuf,
    cache: PathBuf,
    source_checkout: PathBuf,
    workspace_id: String,
    gen1: String,
    gen2: String,
}

fn linear_repo(workspace_id: &str, kind: AttachmentPolicyKind) -> LinearRepo {
    let root = TempDir::new().unwrap();
    let registry = open_registry(root.path());
    let binding = bind(&registry, root.path(), workspace_id);
    let store = bundle_store(&registry, &binding);
    seed_one(
        &store,
        &registry,
        workspace_id,
        "ORB-00001",
        if kind == AttachmentPolicyKind::Fail {
            &[]
        } else {
            &[("notes.txt", b"hello")]
        },
    );

    let snap1 = root.path().join("snap-1");
    build_publication_snapshot(
        &registry,
        &snap1,
        metadata(workspace_id, 1, None),
        &policy(kind),
        None,
    )
    .unwrap();
    let remote = root.path().join("publication.git");
    init_repo(&remote);
    let gen1 = commit_snapshot(&remote, &snap1, "generation 1");

    let snap2 = root.path().join("snap-2");
    build_publication_snapshot(
        &registry,
        &snap2,
        metadata(workspace_id, 2, Some(&gen1)),
        &policy(kind),
        None,
    )
    .unwrap();
    let gen2 = commit_snapshot(&remote, &snap2, "generation 2");

    let source_checkout = root.path().join("source-checkout");
    init_repo(&source_checkout);
    fs::write(source_checkout.join("README.md"), "source\n").unwrap();
    git(&source_checkout, &["add", "README.md"]);
    git(&source_checkout, &["commit", "-m", "source"]);

    LinearRepo {
        cache: root.path().join("consumer-cache"),
        remote,
        source_checkout,
        workspace_id: workspace_id.to_string(),
        gen1,
        gen2,
        root,
    }
}

fn assert_label(
    inspection: &PublicationInspection,
    workspace_id: &str,
    generation: u64,
    commit: &str,
    freshness: PublicationFreshness,
    incomplete: bool,
) {
    let label = &inspection.label;
    assert_eq!(label.workspace_id, workspace_id);
    assert_eq!(label.generation, generation);
    assert_eq!(label.commit_id, commit);
    assert_eq!(
        label.source_repository_fingerprint,
        "git@github.com:example/orbit-source.git"
    );
    assert_eq!(label.authority_machine_id, "hm_owner");
    assert_eq!(label.publication_id, "pub_orbit_primary");
    assert_eq!(label.freshness, freshness);
    assert_eq!(label.incomplete_attachments, incomplete);
    assert_eq!(label.render_authority, PublicationRenderAuthority::Snapshot);
    assert!(!inspection.tasks.is_empty());
    for task in &inspection.tasks {
        assert_eq!(task.label, *label);
        assert_eq!(
            task.label.render_authority,
            PublicationRenderAuthority::Snapshot
        );
    }
}

#[test]
fn current_snapshot_is_labelled_and_not_live() {
    let repo = linear_repo("ws_inspect_current", AttachmentPolicyKind::Fail);
    let inspection =
        inspect_publication(request(&repo.workspace_id, &repo.remote, &repo.cache, None)).unwrap();
    assert_label(
        &inspection,
        &repo.workspace_id,
        2,
        &repo.gen2,
        PublicationFreshness::Current,
        false,
    );
    assert_eq!(inspection.git_parent.as_deref(), Some(repo.gen1.as_str()));
    assert_eq!(inspection.tasks[0].task.id, "ORB-00001");
}

#[test]
fn stale_snapshot_is_labelled_with_older_generation() {
    let repo = linear_repo("ws_inspect_stale", AttachmentPolicyKind::Fail);
    let inspection = inspect_publication(request(
        &repo.workspace_id,
        &repo.remote,
        &repo.cache,
        Some(&repo.gen1),
    ))
    .unwrap();
    assert_label(
        &inspection,
        &repo.workspace_id,
        1,
        &repo.gen1,
        PublicationFreshness::Stale,
        false,
    );
    assert_eq!(inspection.git_parent, None);
}

#[test]
fn omit_projection_is_labelled_incomplete() {
    let repo = linear_repo("ws_inspect_omit", AttachmentPolicyKind::Omit);
    let inspection =
        inspect_publication(request(&repo.workspace_id, &repo.remote, &repo.cache, None)).unwrap();
    assert_label(
        &inspection,
        &repo.workspace_id,
        2,
        &repo.gen2,
        PublicationFreshness::Current,
        true,
    );
    assert!(!inspection.envelope.omitted_attachments.is_empty());
}

#[test]
fn tampered_bundle_and_jsonl_fail_before_trusted_state() {
    let repo = linear_repo("ws_inspect_tamper", AttachmentPolicyKind::Include);
    let blob = repo
        .remote
        .join("tasks/ORB-00001/artifacts/files/notes.txt");
    fs::write(&blob, b"tampered-secret-content").unwrap();
    amend_current(&repo.remote);
    let error = inspect_publication(request(&repo.workspace_id, &repo.remote, &repo.cache, None))
        .unwrap_err()
        .to_string();
    assert!(error.contains("ORB-00001"));
    assert!(!error.contains("tampered-secret-content"));

    git(&repo.remote, &["reset", "--hard", &repo.gen2]);
    let events = repo
        .remote
        .join("tasks/ORB-00001")
        .join(TASK_EVENTS_FILE_NAME);
    let raw = fs::read_to_string(&events).unwrap();
    fs::write(&events, format!("{raw}{{")).unwrap();
    amend_current(&repo.remote);
    let jsonl_error = inspect_publication(request(
        &repo.workspace_id,
        &repo.remote,
        &repo.cache.join("jsonl"),
        None,
    ))
    .unwrap_err()
    .to_string();
    assert!(jsonl_error.contains("ORB-00001"));
    assert!(jsonl_error.contains("events.jsonl"));
}

#[test]
fn mismatched_repository_and_branch_fail_closed() {
    let repo = linear_repo("ws_inspect_mismatch", AttachmentPolicyKind::Fail);
    let mut wrong_workspace = request(&repo.workspace_id, &repo.remote, &repo.cache, None);
    wrong_workspace.workspace_id = "ws_other".to_string();
    assert!(
        inspect_publication(wrong_workspace)
            .unwrap_err()
            .to_string()
            .contains("workspace mismatch")
    );

    let mut wrong_source = request(
        &repo.workspace_id,
        &repo.remote,
        &repo.cache.join("source"),
        None,
    );
    wrong_source.source_repository_fingerprint =
        "git@github.com:example/other-source.git".to_string();
    assert!(
        inspect_publication(wrong_source)
            .unwrap_err()
            .to_string()
            .contains("source repository fingerprint mismatch")
    );

    let mut wrong_lineage = request(
        &repo.workspace_id,
        &repo.remote,
        &repo.cache.join("lineage"),
        None,
    );
    wrong_lineage.publication_id = "pub_other".to_string();
    assert!(
        inspect_publication(wrong_lineage)
            .unwrap_err()
            .to_string()
            .contains("publication id mismatch")
    );

    let mut wrong_authority = request(
        &repo.workspace_id,
        &repo.remote,
        &repo.cache.join("authority"),
        None,
    );
    wrong_authority.authority_machine_id = "hm_other".to_string();
    assert!(
        inspect_publication(wrong_authority)
            .unwrap_err()
            .to_string()
            .contains("authority mismatch")
    );

    let mut wrong_branch = request(
        &repo.workspace_id,
        &repo.remote,
        &repo.cache.join("branch"),
        None,
    );
    wrong_branch.publication_branch = "refs/heads/other".to_string();
    let branch_error = inspect_publication(wrong_branch).unwrap_err().to_string();
    assert!(
        branch_error.contains("branch") || branch_error.contains("git"),
        "{branch_error}"
    );
}

#[test]
fn unsupported_future_schemas_fail_closed() {
    let repo = linear_repo("ws_inspect_schema", AttachmentPolicyKind::Fail);
    let envelope_path = repo.remote.join(PUBLICATION_ENVELOPE_FILE_NAME);
    let yaml = fs::read_to_string(&envelope_path)
        .unwrap()
        .replace("format_version: 1", "format_version: 9");
    fs::write(&envelope_path, yaml).unwrap();
    amend_current(&repo.remote);
    let envelope_error =
        inspect_publication(request(&repo.workspace_id, &repo.remote, &repo.cache, None))
            .unwrap_err()
            .to_string();
    assert!(envelope_error.contains("unsupported task publication format version 9"));

    git(&repo.remote, &["reset", "--hard", &repo.gen2]);
    let task_yaml = repo.remote.join("tasks/ORB-00001/task.yaml");
    fs::write(
        &task_yaml,
        "schema_version: 999\nid: ORB-00001\ntitle: future\n",
    )
    .unwrap();
    amend_current(&repo.remote);
    let task_error = inspect_publication(request(
        &repo.workspace_id,
        &repo.remote,
        &repo.cache.join("task-schema"),
        None,
    ))
    .unwrap_err()
    .to_string();
    assert!(task_error.contains("ORB-00001"));
}

#[test]
fn parent_mismatch_fails_closed() {
    let repo = linear_repo("ws_inspect_parent", AttachmentPolicyKind::Fail);
    let envelope_path = repo.remote.join(PUBLICATION_ENVELOPE_FILE_NAME);
    let yaml = fs::read_to_string(&envelope_path)
        .unwrap()
        .replace(&repo.gen1, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    fs::write(&envelope_path, yaml).unwrap();
    git(&repo.remote, &["add", "-A"]);
    git(&repo.remote, &["commit", "-m", "wrong parent"]);
    let error = inspect_publication(request(&repo.workspace_id, &repo.remote, &repo.cache, None))
        .unwrap_err()
        .to_string();
    assert!(error.contains("previous-publication"));
}

#[test]
fn inspect_does_not_mutate_canonical_or_source_state() {
    let repo = linear_repo("ws_inspect_readonly", AttachmentPolicyKind::Fail);
    let registry = open_registry(repo.root.path());
    let before_tasks = registry
        .tasks_for_workspace(&repo.workspace_id)
        .unwrap()
        .len();
    let before_allocator = registry.allocator_next_number().unwrap();
    let canonical = registry
        .canonical_task_bundle_path(&repo.workspace_id, "ORB-00001")
        .unwrap();
    let before_bundle = tree_bytes(&canonical);
    let before_source = tree_bytes(&repo.source_checkout);
    let before_source_head = git(&repo.source_checkout, &["rev-parse", "HEAD"]);
    let before_source_status = git(&repo.source_checkout, &["status", "--porcelain"]);
    let checkout = repo
        .root
        .path()
        .join("repos")
        .join(&repo.workspace_id)
        .join(".orbit");
    let before_checkout = tree_bytes(&checkout);

    inspect_publication(request(&repo.workspace_id, &repo.remote, &repo.cache, None)).unwrap();

    assert_eq!(
        registry
            .tasks_for_workspace(&repo.workspace_id)
            .unwrap()
            .len(),
        before_tasks
    );
    assert_eq!(registry.allocator_next_number().unwrap(), before_allocator);
    assert_eq!(tree_bytes(&canonical), before_bundle);
    assert_eq!(tree_bytes(&repo.source_checkout), before_source);
    assert_eq!(
        git(&repo.source_checkout, &["rev-parse", "HEAD"]),
        before_source_head
    );
    assert_eq!(
        git(&repo.source_checkout, &["status", "--porcelain"]),
        before_source_status
    );
    assert_eq!(tree_bytes(&checkout), before_checkout);
    assert!(!repo.root.path().join("claims").exists());
    assert!(!repo.root.path().join("audit").exists());
    assert!(!repo.root.path().join("runs").exists());
}

#[test]
fn inspect_preserves_crlf_bytes_and_skips_configured_filters() {
    let root = TempDir::new().unwrap();
    let registry = open_registry(root.path());
    let workspace_id = "ws_inspect_crlf_attr";
    let binding = bind(&registry, root.path(), workspace_id);
    let store = bundle_store(&registry, &binding);
    seed_one(&store, &registry, workspace_id, "ORB-00001", &[]);
    seed_crlf_gitattributes_attachments(&store, "ORB-00001");

    let snap = root.path().join("snap");
    build_publication_snapshot(
        &registry,
        &snap,
        metadata(workspace_id, 1, None),
        &policy(AttachmentPolicyKind::Include),
        None,
    )
    .unwrap();

    let remote = root.path().join("publication.git");
    init_repo(&remote);
    replace_worktree(&remote, &snap);
    git_add_literal(&remote);
    git(&remote, &["commit", "-m", "generation 1"]);
    let commit = git(&remote, &["rev-parse", "HEAD"]);

    let cache = root.path().join("consumer-cache");
    let (env, sentinel) = poison_publication_git_filters(root.path());
    let snapshot =
        load_validated_publication(request(workspace_id, &remote, &cache, None)).unwrap();
    drop(env);

    assert_label(
        &snapshot.inspection,
        workspace_id,
        1,
        &commit,
        PublicationFreshness::Current,
        false,
    );
    assert!(
        !sentinel.exists(),
        "inspection executed an external Git filter"
    );
    let bundle_dir = &snapshot.bundles[0].source_dir;
    assert_eq!(
        fs::read(bundle_dir.join("artifacts/files/payload.txt")).unwrap(),
        CRLF_PAYLOAD
    );
    read_bundle_at(bundle_dir).expect("inspected bundle validates");
}

/// Two generations whose single task differs in title and attachment bytes, so
/// content read from the wrong commit is observable.
struct DivergingRepo {
    root: TempDir,
    remote: PathBuf,
    cache: PathBuf,
    workspace_id: String,
    gen1: String,
    gen2: String,
}

const GEN1_TITLE: &str = "generation one";
const GEN2_TITLE: &str = "generation two";
const GEN1_BYTES: &[u8] = b"generation one bytes";
const GEN2_BYTES: &[u8] = b"generation two bytes";

fn diverging_repo(workspace_id: &str) -> DivergingRepo {
    let root = TempDir::new().unwrap();
    let remote = root.path().join("publication.git");
    init_repo(&remote);
    let mut commits = Vec::new();
    for (generation, title, bytes) in [(1, GEN1_TITLE, GEN1_BYTES), (2, GEN2_TITLE, GEN2_BYTES)] {
        let source = root.path().join(format!("source-{generation}"));
        let registry = open_registry(&source);
        let binding = bind(&registry, &source, workspace_id);
        let store = bundle_store(&registry, &binding);
        seed(
            &store,
            &registry,
            workspace_id,
            &make_bundle("ORB-00001", title, Vec::new()),
        );
        let entry = seed_artifact_blob(&store, "ORB-00001", "notes.txt", bytes, "codex");
        store
            .rewrite_artifact_manifest(
                "ORB-00001",
                &ArtifactManifestV2 {
                    schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
                    files: vec![entry],
                },
            )
            .unwrap();
        let snapshot = root.path().join(format!("snap-{generation}"));
        build_publication_snapshot(
            &registry,
            &snapshot,
            metadata(workspace_id, generation, commits.last().map(String::as_str)),
            &policy(AttachmentPolicyKind::Include),
            None,
        )
        .unwrap();
        commits.push(commit_snapshot(
            &remote,
            &snapshot,
            &format!("generation {generation}"),
        ));
    }
    let gen2 = commits.pop().unwrap();
    let gen1 = commits.pop().unwrap();
    DivergingRepo {
        cache: root.path().join("consumer-cache"),
        remote,
        workspace_id: workspace_id.to_string(),
        gen1,
        gen2,
        root,
    }
}

/// A publication read running on its own thread, parked at one checkpoint
/// until the test has run the interleaved read to completion.
struct PausedRead<T> {
    paused: mpsc::Receiver<()>,
    resume: mpsc::Sender<()>,
    handle: JoinHandle<T>,
}

impl<T: Send + 'static> PausedRead<T> {
    fn start(at: SnapshotCheckpoint, read: impl FnOnce() -> T + Send + 'static) -> Self {
        let (paused_tx, paused) = mpsc::channel();
        let (resume, resume_rx) = mpsc::channel::<()>();
        let handle = thread::spawn(move || {
            set_snapshot_checkpoint_hook(Some(Box::new(move |reached| {
                if reached == at {
                    paused_tx.send(()).unwrap();
                    resume_rx
                        .recv_timeout(CHECKPOINT_WAIT)
                        .expect("the interleaved read must not wait on the paused one");
                }
            })));
            let result = read();
            set_snapshot_checkpoint_hook(None);
            result
        });
        let read = Self {
            paused,
            resume,
            handle,
        };
        read.paused
            .recv_timeout(CHECKPOINT_WAIT)
            .expect("paused read reaches its checkpoint");
        read
    }

    fn finish(self) -> T {
        self.resume.send(()).unwrap();
        self.handle.join().unwrap()
    }
}

fn private_trees_left(repo: &DivergingRepo) -> usize {
    fs::read_dir(repo.cache.join("pub_orbit_primary").join("trees"))
        .unwrap()
        .count()
}

#[test]
fn concurrent_inspections_of_two_commits_keep_their_own_snapshot() {
    let repo = diverging_repo("ws_inspect_concurrent");
    let generations = [
        (
            repo.gen1.clone(),
            1,
            GEN1_TITLE,
            PublicationFreshness::Stale,
        ),
        (
            repo.gen2.clone(),
            2,
            GEN2_TITLE,
            PublicationFreshness::Current,
        ),
    ];
    for (paused_index, interleaved_index) in [(0, 1), (1, 0)] {
        let (paused_commit, paused_generation, paused_title, paused_freshness) =
            &generations[paused_index];
        let (other_commit, other_generation, other_title, other_freshness) =
            &generations[interleaved_index];
        let paused_request = request(
            &repo.workspace_id,
            &repo.remote,
            &repo.cache,
            Some(paused_commit),
        );
        let paused = PausedRead::start(SnapshotCheckpoint::EnvelopeValidated, move || {
            inspect_publication(paused_request)
        });
        // The paused read holds its envelope but has not read bundles yet; the
        // other commit is fetched and checked out through the same cache now.
        let other = inspect_publication(request(
            &repo.workspace_id,
            &repo.remote,
            &repo.cache,
            Some(other_commit),
        ))
        .unwrap();
        let paused = paused.finish().unwrap();

        assert_label(
            &paused,
            &repo.workspace_id,
            *paused_generation,
            paused_commit,
            *paused_freshness,
            false,
        );
        assert_eq!(paused.tasks[0].task.title, *paused_title);
        assert_label(
            &other,
            &repo.workspace_id,
            *other_generation,
            other_commit,
            *other_freshness,
            false,
        );
        assert_eq!(other.tasks[0].task.title, *other_title);
    }
    assert_eq!(
        private_trees_left(&repo),
        0,
        "each private checkout is removed with its inspection"
    );
}

#[test]
fn concurrent_inspection_cannot_replace_restore_source_bytes() {
    let repo = diverging_repo("ws_inspect_restore_race");
    let destination = repo.root.path().join("destination");
    let registry = open_registry(&destination);
    let orbit_dir = destination
        .join("repos")
        .join(&repo.workspace_id)
        .join(".orbit");
    fs::create_dir_all(&orbit_dir).unwrap();
    registry
        .bind_workspace(BindWorkspaceParams {
            partition_id: Some(repo.workspace_id.clone()),
            slug: "restore".to_string(),
            repo_root: orbit_dir.parent().unwrap().to_path_buf(),
            workspace_path: orbit_dir.parent().unwrap().to_path_buf(),
            orbit_dir,
            repo_fingerprint: Some("git@github.com:example/orbit-source.git".to_string()),
        })
        .unwrap();

    let restore_request = PublicationRestoreRequest {
        task_workspace_id: repo.workspace_id.clone(),
        publication: request(
            &repo.workspace_id,
            &repo.remote,
            &repo.cache,
            Some(&repo.gen1),
        ),
        mode: PublicationRestoreMode::EmptyDestination,
    };
    let restore_registry = registry.clone();
    let paused = PausedRead::start(SnapshotCheckpoint::BundlesValidated, move || {
        restore_publication(&restore_registry, restore_request)
    });
    // Restore has validated generation 1 and not yet staged its artifacts; an
    // inspection of generation 2 runs through the same cache in between.
    let other = inspect_publication(request(
        &repo.workspace_id,
        &repo.remote,
        &repo.cache,
        Some(&repo.gen2),
    ))
    .unwrap();
    assert_eq!(other.tasks[0].task.title, GEN2_TITLE);
    let outcome = paused.finish().unwrap();

    assert_eq!(outcome.generation, 1);
    assert_eq!(outcome.restored_task_ids, ["ORB-00001"]);
    let canonical = registry
        .canonical_task_bundle_path(&repo.workspace_id, "ORB-00001")
        .unwrap();
    let restored = read_bundle_at(&canonical).expect("restored bundle validates");
    assert_eq!(restored.envelope.title, GEN1_TITLE);
    assert_eq!(
        fs::read(canonical.join("artifacts/files/notes.txt")).unwrap(),
        GEN1_BYTES
    );
    assert_eq!(
        private_trees_left(&repo),
        0,
        "restore releases its private checkout once staged"
    );
}
