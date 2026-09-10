use std::fs;
use std::path::{Path, PathBuf};

use orbit_types::task::{ArtifactManifestV2, TASK_ARTIFACT_SCHEMA_VERSION};
use tempfile::TempDir;

use super::*;
use crate::workflow::task::restore::{RestoreFailurePoint, restore_publication_with_failure};

const WORKSPACE: &str = "ws_restore";
const TASK_WORKSPACE: &str = "ws_restore-runtime";
const FINGERPRINT: &str = "git@github.com:example/orbit-source.git";
const PUBLICATION: &str = "pub_orbit_restore";
const AUTHORITY: &str = "hm_owner";

struct RestoreFixture {
    _source: TempDir,
    destination: TempDir,
    remote: PathBuf,
    cache: PathBuf,
    task_workspace_id: String,
}

impl RestoreFixture {
    fn registry(&self) -> TaskRegistryStore {
        open_registry(self.destination.path())
    }

    fn request(&self, mode: PublicationRestoreMode) -> PublicationRestoreRequest {
        PublicationRestoreRequest {
            task_workspace_id: self.task_workspace_id.clone(),
            publication: PublicationInspectRequest {
                workspace_id: WORKSPACE.to_string(),
                source_repository_fingerprint: FINGERPRINT.to_string(),
                publication_id: PUBLICATION.to_string(),
                authority_machine_id: AUTHORITY.to_string(),
                publication_remote: self.remote.to_string_lossy().into_owned(),
                publication_branch: "refs/heads/main".to_string(),
                cache_dir: self.cache.clone(),
                commit: None,
            },
            mode,
        }
    }
}

fn bind_restore(
    registry: &TaskRegistryStore,
    root: &Path,
    workspace_id: &str,
) -> WorkspaceCheckoutBinding {
    let orbit_dir = root.join("repos").join(workspace_id).join(".orbit");
    fs::create_dir_all(&orbit_dir).unwrap();
    registry
        .bind_workspace(BindWorkspaceParams {
            workspace_id: Some(workspace_id.to_string()),
            slug: "restore".to_string(),
            repo_root: orbit_dir.parent().unwrap().to_path_buf(),
            workspace_path: orbit_dir.parent().unwrap().to_path_buf(),
            orbit_dir,
            repo_fingerprint: Some(FINGERPRINT.to_string()),
        })
        .unwrap()
}

fn fixture(kind: AttachmentPolicyKind) -> RestoreFixture {
    fixture_with_task_workspace(kind, WORKSPACE)
}

fn fixture_with_task_workspace(
    kind: AttachmentPolicyKind,
    task_workspace_id: &str,
) -> RestoreFixture {
    let source = TempDir::new().unwrap();
    let source_registry = open_registry(source.path());
    let source_binding = bind_restore(&source_registry, source.path(), WORKSPACE);
    let store = bundle_store(&source_registry, &source_binding);
    seed(
        &store,
        &source_registry,
        WORKSPACE,
        &make_bundle("ORB-00001", "first", Vec::new()),
    );
    seed(
        &store,
        &source_registry,
        WORKSPACE,
        &make_bundle("ORB-00007", "seventh", Vec::new()),
    );
    let entry = seed_artifact_blob(&store, "ORB-00007", "report.txt", b"recovery", "codex");
    store
        .rewrite_artifact_manifest(
            "ORB-00007",
            &ArtifactManifestV2 {
                schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
                files: vec![entry],
            },
        )
        .unwrap();

    let snapshot = source.path().join("snapshot");
    build_publication_snapshot(
        &source_registry,
        &snapshot,
        PublicationSnapshotMetadata {
            publication_id: PUBLICATION.to_string(),
            workspace_id: WORKSPACE.to_string(),
            source_repository_fingerprint: FINGERPRINT.to_string(),
            authority_machine_id: AUTHORITY.to_string(),
            generation: 1,
            published_at: Utc.with_ymd_and_hms(2026, 8, 30, 4, 0, 0).unwrap(),
            previous_publication: None,
        },
        &AttachmentPolicy {
            kind,
            max_file_bytes: 1024,
            max_total_bytes: 4096,
            deny_patterns: Vec::new(),
            scanner_failure_behavior: ScannerFailureBehavior::AllowUnchecked,
        },
        None,
    )
    .unwrap();

    let remote = source.path().join("publication.git");
    fs::create_dir_all(&remote).unwrap();
    git(&remote, &["init", "-b", "main"]);
    replace_worktree(&remote, &snapshot);
    git_add_literal(&remote);
    git(&remote, &["commit", "-m", "publication"]);

    let destination = TempDir::new().unwrap();
    let destination_registry = open_registry(destination.path());
    bind_restore(&destination_registry, destination.path(), task_workspace_id);
    let cache = destination.path().join("publication-cache");
    RestoreFixture {
        _source: source,
        destination,
        remote,
        cache,
        task_workspace_id: task_workspace_id.to_string(),
    }
}

#[test]
fn empty_destination_restore_preserves_ids_without_checkout_projection() {
    let fixture = fixture(AttachmentPolicyKind::Include);
    let registry = fixture.registry();
    let outcome = restore_publication(
        &registry,
        fixture.request(PublicationRestoreMode::EmptyDestination),
    )
    .unwrap();

    assert_eq!(outcome.restored_task_ids, ["ORB-00001", "ORB-00007"]);
    assert!(outcome.already_present_task_ids.is_empty());
    assert_eq!(
        outcome.completeness,
        PublicationRecoveryCompleteness::Complete
    );
    assert!(outcome.omitted_attachments.is_empty());
    assert_eq!(registry.allocator_next_number().unwrap(), 8);
    assert_eq!(registry.tasks_for_workspace(WORKSPACE).unwrap().len(), 2);
    for task_id in &outcome.restored_task_ids {
        let restored = read_bundle_at(
            &registry
                .canonical_task_bundle_path(WORKSPACE, task_id)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(restored.envelope.id, *task_id);
    }
    assert!(
        !fixture
            .destination
            .path()
            .join("repos/ws_restore/.orbit/tasks")
            .exists()
    );
}

#[test]
fn restore_keeps_logical_identity_but_writes_to_the_runtime_task_partition() {
    let fixture = fixture_with_task_workspace(AttachmentPolicyKind::Include, TASK_WORKSPACE);
    let registry = fixture.registry();
    let outcome = restore_publication(
        &registry,
        fixture.request(PublicationRestoreMode::EmptyDestination),
    )
    .unwrap();

    assert_eq!(outcome.workspace_id, WORKSPACE);
    assert_eq!(
        registry.tasks_for_workspace(TASK_WORKSPACE).unwrap().len(),
        2
    );
    assert!(registry.tasks_for_workspace(WORKSPACE).unwrap().is_empty());
    for task_id in &outcome.restored_task_ids {
        assert!(
            registry
                .canonical_task_bundle_path(TASK_WORKSPACE, task_id)
                .unwrap()
                .is_dir()
        );
        assert!(
            !registry
                .canonical_task_bundle_path(WORKSPACE, task_id)
                .unwrap()
                .exists()
        );
    }
    assert!(
        !fixture
            .destination
            .path()
            .join("repos/ws_restore-runtime/.orbit/tasks")
            .exists()
    );
}

#[test]
fn identical_retry_is_explicit_and_does_not_duplicate_or_advance() {
    let fixture = fixture(AttachmentPolicyKind::Include);
    let registry = fixture.registry();
    restore_publication(
        &registry,
        fixture.request(PublicationRestoreMode::EmptyDestination),
    )
    .unwrap();
    let allocator = registry.allocator_next_number().unwrap();
    let canonical = registry
        .canonical_task_bundle_path(WORKSPACE, "ORB-00007")
        .unwrap();
    let before = tree_bytes(&canonical);

    let retry = restore_publication(
        &registry,
        fixture.request(PublicationRestoreMode::AllowIdenticalRetry),
    )
    .unwrap();
    assert!(retry.restored_task_ids.is_empty());
    assert_eq!(retry.already_present_task_ids, ["ORB-00001", "ORB-00007"]);
    assert_eq!(registry.allocator_next_number().unwrap(), allocator);
    assert_eq!(tree_bytes(&canonical), before);
    assert_eq!(registry.tasks_for_workspace(WORKSPACE).unwrap().len(), 2);
}

#[test]
fn restore_preserves_crlf_attachment_bytes_despite_gitattributes() {
    let source = TempDir::new().unwrap();
    let source_registry = open_registry(source.path());
    let source_binding = bind_restore(&source_registry, source.path(), WORKSPACE);
    let store = bundle_store(&source_registry, &source_binding);
    seed(
        &store,
        &source_registry,
        WORKSPACE,
        &make_bundle("ORB-00001", "crlf", Vec::new()),
    );
    seed_crlf_gitattributes_attachments(&store, "ORB-00001");

    let snapshot = source.path().join("snapshot");
    build_publication_snapshot(
        &source_registry,
        &snapshot,
        PublicationSnapshotMetadata {
            publication_id: PUBLICATION.to_string(),
            workspace_id: WORKSPACE.to_string(),
            source_repository_fingerprint: FINGERPRINT.to_string(),
            authority_machine_id: AUTHORITY.to_string(),
            generation: 1,
            published_at: Utc.with_ymd_and_hms(2026, 8, 30, 4, 0, 0).unwrap(),
            previous_publication: None,
        },
        &include_attachment_policy(),
        None,
    )
    .unwrap();

    let remote = source.path().join("publication.git");
    fs::create_dir_all(&remote).unwrap();
    git(&remote, &["init", "-b", "main"]);
    replace_worktree(&remote, &snapshot);
    git_add_literal(&remote);
    git(&remote, &["commit", "-m", "publication"]);

    let destination = TempDir::new().unwrap();
    let destination_registry = open_registry(destination.path());
    bind_restore(&destination_registry, destination.path(), WORKSPACE);
    let cache = destination.path().join("publication-cache");
    let (env, sentinel) = poison_publication_git_filters(destination.path());
    let outcome = restore_publication(
        &destination_registry,
        PublicationRestoreRequest {
            task_workspace_id: WORKSPACE.to_string(),
            publication: PublicationInspectRequest {
                workspace_id: WORKSPACE.to_string(),
                source_repository_fingerprint: FINGERPRINT.to_string(),
                publication_id: PUBLICATION.to_string(),
                authority_machine_id: AUTHORITY.to_string(),
                publication_remote: remote.to_string_lossy().into_owned(),
                publication_branch: "refs/heads/main".to_string(),
                cache_dir: cache,
                commit: None,
            },
            mode: PublicationRestoreMode::EmptyDestination,
        },
    )
    .unwrap();
    drop(env);

    assert_eq!(outcome.restored_task_ids, ["ORB-00001"]);
    assert!(
        !sentinel.exists(),
        "restore inspection executed an external Git filter"
    );
    let canonical = destination_registry
        .canonical_task_bundle_path(WORKSPACE, "ORB-00001")
        .unwrap();
    assert_eq!(
        fs::read(canonical.join("artifacts/files/payload.txt")).unwrap(),
        CRLF_PAYLOAD
    );
    read_bundle_at(&canonical).expect("restored bundle validates");
}

#[test]
fn restore_rejects_tampered_publication_artifact_bytes() {
    let fixture = fixture(AttachmentPolicyKind::Include);
    let blob = fixture
        .remote
        .join("tasks/ORB-00007/artifacts/files/report.txt");
    fs::write(&blob, b"tampered-secret-content").unwrap();
    git(&fixture.remote, &["add", "-A"]);
    git(&fixture.remote, &["commit", "--amend", "--no-edit"]);
    let registry = fixture.registry();
    let error = restore_publication(
        &registry,
        fixture.request(PublicationRestoreMode::EmptyDestination),
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("ORB-00007"), "{error}");
    assert!(!error.contains("tampered-secret-content"), "{error}");
    assert!(
        registry
            .tasks_for_workspace(&fixture.task_workspace_id)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn collision_and_identity_mismatch_abort_before_mutation() {
    let collision_fixture = fixture(AttachmentPolicyKind::Include);
    let registry = collision_fixture.registry();
    let binding = registry
        .find_workspace_checkout(WORKSPACE)
        .unwrap()
        .unwrap();
    let store = bundle_store(&registry, &binding);
    seed(
        &store,
        &registry,
        WORKSPACE,
        &make_bundle("ORB-00001", "different", Vec::new()),
    );
    let before = tree_bytes(registry.workspaces_dir());
    let allocator = registry.allocator_next_number().unwrap();
    let error = restore_publication(
        &registry,
        collision_fixture.request(PublicationRestoreMode::AllowIdenticalRetry),
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("non-identical"), "{error}");
    assert_eq!(tree_bytes(registry.workspaces_dir()), before);
    assert_eq!(registry.allocator_next_number().unwrap(), allocator);

    let other = fixture(AttachmentPolicyKind::Include);
    let other_registry = other.registry();
    let mut wrong = other.request(PublicationRestoreMode::EmptyDestination);
    wrong.publication.authority_machine_id = "hm_other".to_string();
    assert!(restore_publication(&other_registry, wrong).is_err());
    assert!(
        other_registry
            .tasks_for_workspace(WORKSPACE)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn omitted_attachments_are_reported_as_incomplete_recovery() {
    let fixture = fixture(AttachmentPolicyKind::Omit);
    let outcome = restore_publication(
        &fixture.registry(),
        fixture.request(PublicationRestoreMode::EmptyDestination),
    )
    .unwrap();
    assert_eq!(
        outcome.completeness,
        PublicationRecoveryCompleteness::IncompleteAttachments
    );
    assert_eq!(outcome.omitted_attachments.len(), 1);
    assert_eq!(outcome.omitted_attachments[0].path, "report.txt");
}

#[test]
fn corrupt_included_blob_and_unsupported_schema_leave_destination_empty() {
    let corrupt_fixture = fixture(AttachmentPolicyKind::Include);
    fs::write(
        corrupt_fixture
            .remote
            .join("tasks/ORB-00007/artifacts/files/report.txt"),
        b"corrupt",
    )
    .unwrap();
    git(&corrupt_fixture.remote, &["add", "-A"]);
    git(&corrupt_fixture.remote, &["commit", "--amend", "--no-edit"]);
    let registry = corrupt_fixture.registry();
    assert!(
        restore_publication(
            &registry,
            corrupt_fixture.request(PublicationRestoreMode::EmptyDestination)
        )
        .is_err()
    );
    assert!(registry.tasks_for_workspace(WORKSPACE).unwrap().is_empty());

    let future = fixture(AttachmentPolicyKind::Omit);
    let envelope = future.remote.join(PUBLICATION_ENVELOPE_FILE_NAME);
    let raw = fs::read_to_string(&envelope)
        .unwrap()
        .replace("format_version: 1", "format_version: 99");
    fs::write(&envelope, raw).unwrap();
    git(&future.remote, &["add", "-A"]);
    git(&future.remote, &["commit", "--amend", "--no-edit"]);
    let future_registry = future.registry();
    assert!(
        restore_publication(
            &future_registry,
            future.request(PublicationRestoreMode::EmptyDestination)
        )
        .is_err()
    );
    assert!(
        future_registry
            .tasks_for_workspace(WORKSPACE)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn every_mutation_phase_rolls_back_canonical_registry_and_allocator() {
    for failure in [
        RestoreFailurePoint::BundlePublication,
        RestoreFailurePoint::IndexRebuild,
        RestoreFailurePoint::AllocatorAdvance,
    ] {
        let fixture = fixture(AttachmentPolicyKind::Include);
        let registry = fixture.registry();
        let allocator = registry.allocator_next_number().unwrap();
        let error = restore_publication_with_failure(
            &registry,
            fixture.request(PublicationRestoreMode::EmptyDestination),
            failure,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("injected failure"), "{failure:?}: {error}");
        assert!(registry.tasks_for_workspace(WORKSPACE).unwrap().is_empty());
        assert_eq!(registry.allocator_next_number().unwrap(), allocator);
        assert!(
            !fixture
                .destination
                .path()
                .join("repos/ws_restore/.orbit/tasks")
                .exists(),
            "{failure:?}"
        );
        for task_id in ["ORB-00001", "ORB-00007"] {
            assert!(
                !registry
                    .canonical_task_bundle_path(WORKSPACE, task_id)
                    .unwrap()
                    .exists(),
                "{failure:?}: {task_id}"
            );
        }
    }
}
