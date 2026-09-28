use std::collections::BTreeMap;

use orbit_common::OrbitError;
use orbit_types::task::{TASK_EVENTS_FILE_NAME, TaskEventRowV2, TaskStatus};
use sha2::Digest;

use crate::driver::file::task_bundle::{
    BundleWriteFault, PENDING_WRITE_FILE_NAME, PendingWriteGuard, TaskDocumentV2,
    inject_bundle_write_faults,
};

use super::*;

struct ClearScanner;

impl AttachmentSensitivityScanner for ClearScanner {
    fn scan(
        &self,
        _input: AttachmentScanInput<'_>,
    ) -> Result<AttachmentScanOutcome, AttachmentScanFailure> {
        Ok(AttachmentScanOutcome::Clear)
    }
}

struct SensitiveScanner;

impl AttachmentSensitivityScanner for SensitiveScanner {
    fn scan(
        &self,
        _input: AttachmentScanInput<'_>,
    ) -> Result<AttachmentScanOutcome, AttachmentScanFailure> {
        Ok(AttachmentScanOutcome::Sensitive)
    }
}

struct FailedScanner;

impl AttachmentSensitivityScanner for FailedScanner {
    fn scan(
        &self,
        _input: AttachmentScanInput<'_>,
    ) -> Result<AttachmentScanOutcome, AttachmentScanFailure> {
        Err(AttachmentScanFailure::Failed)
    }
}

fn metadata(workspace_id: &str) -> PublicationSnapshotMetadata {
    PublicationSnapshotMetadata {
        publication_id: "pub_orbit_primary".to_string(),
        workspace_id: workspace_id.to_string(),
        source_repository_fingerprint: "git@github.com:example/orbit-source.git".to_string(),
        authority_machine_id: "hm_owner".to_string(),
        generation: 7,
        published_at: Utc.with_ymd_and_hms(2026, 8, 30, 1, 2, 3).unwrap(),
        previous_publication: Some("a".repeat(40)),
    }
}

fn policy(kind: AttachmentPolicyKind) -> AttachmentPolicy {
    AttachmentPolicy {
        kind,
        max_file_bytes: 1024,
        max_total_bytes: 4096,
        deny_patterns: vec!["**/.env".to_string(), "**/*.pem".to_string()],
        scanner_failure_behavior: ScannerFailureBehavior::Reject,
    }
}

fn seed_artifacts(
    store: &TaskBundleStoreV2,
    registry: &TaskRegistryStore,
    workspace_id: &str,
    task_id: &str,
    files: &[(&str, &[u8])],
) -> Vec<ArtifactManifestFileV2> {
    seed(
        store,
        registry,
        workspace_id,
        &make_bundle(task_id, "publication fixture", Vec::new()),
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
                files: entries.clone(),
            },
        )
        .unwrap();
    entries
}

fn tree_bytes(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn visit(root: &Path, path: &Path, output: &mut BTreeMap<String, Vec<u8>>) {
        let mut entries: Vec<_> = fs::read_dir(path)
            .unwrap()
            .map(|entry| entry.unwrap())
            .collect();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                visit(root, &path, output);
            } else {
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

#[cfg(unix)]
#[test]
fn publication_snapshot_directories_are_private_under_permissive_umask() {
    use std::os::unix::fs::PermissionsExt;

    const CHILD_MARKER: &str = "ORBIT_TEST_PRIVATE_PUBLICATION_DIRECTORIES";
    if std::env::var_os(CHILD_MARKER).is_none() {
        let status = std::process::Command::new("sh")
            .args(["-c", "umask 000; exec \"$@\"", "sh"])
            .arg(std::env::current_exe().expect("current test executable"))
            .arg("publication_snapshot_directories_are_private_under_permissive_umask")
            .env(CHILD_MARKER, "1")
            .status()
            .expect("run test under permissive umask");
        assert!(status.success(), "permissive-umask child failed");
        return;
    }

    let root = TempDir::new().expect("tempdir");
    let registry = open_registry(root.path());
    let workspace_id = "ws_private_publication";
    let binding = bind(&registry, root.path(), workspace_id);
    let store = bundle_store(&registry, &binding);
    seed(
        &store,
        &registry,
        workspace_id,
        &make_bundle("ORB-00001", "private publication", Vec::new()),
    );
    let snapshot = root.path().join("snapshot");
    build_publication_snapshot(
        &registry,
        &snapshot,
        metadata(workspace_id),
        &policy(AttachmentPolicyKind::Fail),
        None,
    )
    .expect("build publication snapshot");

    let mut pending = vec![snapshot];
    while let Some(directory) = pending.pop() {
        let mode = fs::metadata(&directory)
            .expect("publication directory metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700, "{} has mode {mode:04o}", directory.display());
        for entry in fs::read_dir(&directory).expect("read publication directory") {
            let entry = entry.expect("publication entry");
            if entry.file_type().expect("publication entry type").is_dir() {
                pending.push(entry.path());
            }
        }
    }
}

#[test]
fn publication_envelope_round_trips_all_identity_and_projection_fields() {
    let envelope = PublicationEnvelope {
        format_version: TASK_PUBLICATION_FORMAT_VERSION,
        publication_id: "pub_orbit_primary".to_string(),
        workspace_id: "ws_orbit".to_string(),
        source_repository_fingerprint: "git@github.com:example/orbit-source.git".to_string(),
        authority_machine_id: "hm_owner".to_string(),
        generation: 7,
        published_at: Utc.with_ymd_and_hms(2026, 8, 30, 1, 2, 3).unwrap(),
        task_schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
        previous_publication: Some("a".repeat(40)),
        attachment_policy: AttachmentPolicyKind::Omit,
        task_ids: vec!["ORB-00001".to_string(), "ORB-00002".to_string()],
        omitted_attachments: vec![OmittedAttachment {
            task_id: "ORB-00002".to_string(),
            path: "reports/result.txt".to_string(),
            size_bytes: 12,
            sha256: "b".repeat(64),
        }],
    };

    let yaml = envelope.to_yaml().unwrap();
    assert_eq!(PublicationEnvelope::from_yaml(&yaml).unwrap(), envelope);
    assert!(!yaml.contains("checkout"));
    assert!(!yaml.contains("credential"));
}

#[test]
fn no_artifact_snapshots_have_stable_sorted_host_independent_content() {
    let root = TempDir::new().unwrap();
    let registry = open_registry(root.path());
    let workspace_id = "ws_pub_stable";
    let binding = bind(&registry, root.path(), workspace_id);
    let store = bundle_store(&registry, &binding);
    seed(
        &store,
        &registry,
        workspace_id,
        &make_bundle("ORB-00002", "second", Vec::new()),
    );
    seed(
        &store,
        &registry,
        workspace_id,
        &make_bundle("ORB-00001", "first", Vec::new()),
    );

    let first = root.path().join("snapshot-one");
    let second = root.path().join("snapshot-two");
    let outcome = build_publication_snapshot(
        &registry,
        &first,
        metadata(workspace_id),
        &policy(AttachmentPolicyKind::Fail),
        None,
    )
    .unwrap();
    build_publication_snapshot(
        &registry,
        &second,
        metadata(workspace_id),
        &policy(AttachmentPolicyKind::Fail),
        None,
    )
    .unwrap();

    assert_eq!(outcome.envelope.task_ids, vec!["ORB-00001", "ORB-00002"]);
    assert_eq!(tree_bytes(&first), tree_bytes(&second));
    assert!(first.join(PUBLICATION_ENVELOPE_FILE_NAME).is_file());
    assert!(
        first
            .join(PUBLICATION_TASKS_DIR_NAME)
            .join("ORB-00001")
            .join("task.yaml")
            .is_file()
    );
}

#[test]
fn include_copies_validated_artifacts_and_canonicalizes_manifest_order() {
    let root = TempDir::new().unwrap();
    let registry = open_registry(root.path());
    let workspace_id = "ws_pub_include";
    let binding = bind(&registry, root.path(), workspace_id);
    let store = bundle_store(&registry, &binding);
    seed_artifacts(
        &store,
        &registry,
        workspace_id,
        "ORB-00001",
        &[("z.txt", b"last"), ("a.txt", b"first")],
    );

    let destination = root.path().join("included");
    let outcome = build_publication_snapshot(
        &registry,
        &destination,
        metadata(workspace_id),
        &policy(AttachmentPolicyKind::Include),
        Some(&ClearScanner),
    )
    .unwrap();
    assert_eq!(outcome.included_attachment_bytes, 9);
    assert_eq!(outcome.omitted_attachment_bytes, 0);
    let published = read_bundle_at(
        &destination
            .join(PUBLICATION_TASKS_DIR_NAME)
            .join("ORB-00001"),
    )
    .unwrap();
    let paths: Vec<_> = published
        .artifact_manifest
        .unwrap()
        .files
        .into_iter()
        .map(|file| file.path)
        .collect();
    assert_eq!(paths, vec!["a.txt", "z.txt"]);
}

#[test]
fn omit_removes_manifest_and_blobs_and_records_sorted_ledger() {
    let root = TempDir::new().unwrap();
    let registry = open_registry(root.path());
    let workspace_id = "ws_pub_omit";
    let binding = bind(&registry, root.path(), workspace_id);
    let store = bundle_store(&registry, &binding);
    seed_artifacts(
        &store,
        &registry,
        workspace_id,
        "ORB-00002",
        &[("z.txt", b"last"), ("a.txt", b"first")],
    );
    seed(
        &store,
        &registry,
        workspace_id,
        &make_bundle("ORB-00001", "without artifact", Vec::new()),
    );

    let destination = root.path().join("omitted");
    let outcome = build_publication_snapshot(
        &registry,
        &destination,
        metadata(workspace_id),
        &policy(AttachmentPolicyKind::Omit),
        None,
    )
    .unwrap();
    let paths: Vec<_> = outcome
        .envelope
        .omitted_attachments
        .iter()
        .map(|record| record.path.as_str())
        .collect();
    assert_eq!(paths, vec!["a.txt", "z.txt"]);
    assert_eq!(outcome.omitted_attachment_bytes, 9);
    let task_dir = destination
        .join(PUBLICATION_TASKS_DIR_NAME)
        .join("ORB-00002");
    assert!(!task_dir.join("artifacts/manifest.yaml").exists());
    assert_eq!(read_bundle_at(&task_dir).unwrap().artifact_manifest, None);
    assert!(
        tree_bytes(&task_dir)
            .keys()
            .all(|path| !path.contains("files/"))
    );
}

#[test]
fn fail_policy_rejects_any_attachment_without_publishing_destination() {
    let root = TempDir::new().unwrap();
    let registry = open_registry(root.path());
    let workspace_id = "ws_pub_fail";
    let binding = bind(&registry, root.path(), workspace_id);
    let store = bundle_store(&registry, &binding);
    seed_artifacts(
        &store,
        &registry,
        workspace_id,
        "ORB-00001",
        &[("secret.txt", b"do-not-leak-this-content")],
    );
    let destination = root.path().join("rejected");

    let error = build_publication_snapshot(
        &registry,
        &destination,
        metadata(workspace_id),
        &policy(AttachmentPolicyKind::Fail),
        None,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("ORB-00001"));
    assert!(error.contains("secret.txt"));
    assert!(!error.contains("do-not-leak-this-content"));
    assert!(!destination.exists());
}

#[test]
fn include_enforces_path_size_deny_and_sensitivity_policies() {
    let root = TempDir::new().unwrap();
    let registry = open_registry(root.path());
    let workspace_id = "ws_pub_policy";
    let binding = bind(&registry, root.path(), workspace_id);
    let store = bundle_store(&registry, &binding);
    seed_artifacts(
        &store,
        &registry,
        workspace_id,
        "ORB-00001",
        &[("reports/result.txt", b"12345")],
    );

    let mut per_file = policy(AttachmentPolicyKind::Include);
    per_file.max_file_bytes = 4;
    assert!(
        build_publication_snapshot(
            &registry,
            &root.path().join("too-large"),
            metadata(workspace_id),
            &per_file,
            Some(&ClearScanner),
        )
        .unwrap_err()
        .to_string()
        .contains("per-file")
    );

    let mut total = policy(AttachmentPolicyKind::Include);
    total.max_total_bytes = 4;
    assert!(
        build_publication_snapshot(
            &registry,
            &root.path().join("total-large"),
            metadata(workspace_id),
            &total,
            Some(&ClearScanner),
        )
        .unwrap_err()
        .to_string()
        .contains("total")
    );

    let mut denied = policy(AttachmentPolicyKind::Include);
    denied.deny_patterns = vec!["reports/**".to_string()];
    assert!(
        build_publication_snapshot(
            &registry,
            &root.path().join("denied"),
            metadata(workspace_id),
            &denied,
            Some(&ClearScanner),
        )
        .unwrap_err()
        .to_string()
        .contains("deny pattern")
    );

    assert!(
        build_publication_snapshot(
            &registry,
            &root.path().join("sensitive"),
            metadata(workspace_id),
            &policy(AttachmentPolicyKind::Include),
            Some(&SensitiveScanner),
        )
        .unwrap_err()
        .to_string()
        .contains("classified as sensitive")
    );

    let bundle_dir = store.bundle_path("ORB-00001").unwrap();
    fs::write(
        bundle_dir.join("artifacts/manifest.yaml"),
        "schema_version: 1\nfiles:\n  - path: ../escape\n    blob: files/result.txt\n    sha256: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n    media_type: text/plain\n    size_bytes: 5\n    created_by: codex\n    created_at: 2026-08-30T00:00:00Z\n",
    )
    .unwrap();
    let path_error = build_publication_snapshot(
        &registry,
        &root.path().join("bad-path"),
        metadata(workspace_id),
        &policy(AttachmentPolicyKind::Include),
        Some(&ClearScanner),
    )
    .unwrap_err()
    .to_string();
    assert!(path_error.contains("ORB-00001"));
    assert!(path_error.contains("escape"));
}

#[test]
fn scanner_unavailable_and_failed_behavior_is_explicit() {
    let root = TempDir::new().unwrap();
    let registry = open_registry(root.path());
    let workspace_id = "ws_pub_scanner";
    let binding = bind(&registry, root.path(), workspace_id);
    let store = bundle_store(&registry, &binding);
    seed_artifacts(
        &store,
        &registry,
        workspace_id,
        "ORB-00001",
        &[("result.txt", b"safe")],
    );

    for (name, scanner) in [
        ("unavailable", None),
        (
            "failed",
            Some(&FailedScanner as &dyn AttachmentSensitivityScanner),
        ),
    ] {
        let destination = root.path().join(name);
        let error = build_publication_snapshot(
            &registry,
            &destination,
            metadata(workspace_id),
            &policy(AttachmentPolicyKind::Include),
            scanner,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("did not produce a verdict"));
        assert!(!destination.exists());
    }

    let mut allow = policy(AttachmentPolicyKind::Include);
    allow.scanner_failure_behavior = ScannerFailureBehavior::AllowUnchecked;
    build_publication_snapshot(
        &registry,
        &root.path().join("allowed-unchecked"),
        metadata(workspace_id),
        &allow,
        Some(&FailedScanner),
    )
    .unwrap();
}

#[test]
fn tampered_blob_and_invalid_jsonl_tail_leave_destination_unpublished() {
    let root = TempDir::new().unwrap();
    let registry = open_registry(root.path());
    let workspace_id = "ws_pub_tampered";
    let binding = bind(&registry, root.path(), workspace_id);
    let store = bundle_store(&registry, &binding);
    let entries = seed_artifacts(
        &store,
        &registry,
        workspace_id,
        "ORB-00001",
        &[("result.txt", b"original")],
    );
    let bundle_dir = store.bundle_path("ORB-00001").unwrap();
    fs::write(
        bundle_dir
            .join(TASK_ARTIFACTS_DIR_NAME)
            .join(&entries[0].blob),
        b"tampered-secret-content",
    )
    .unwrap();
    let tampered_destination = root.path().join("tampered");
    let error = build_publication_snapshot(
        &registry,
        &tampered_destination,
        metadata(workspace_id),
        &policy(AttachmentPolicyKind::Include),
        Some(&ClearScanner),
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("ORB-00001"));
    assert!(error.contains("result.txt"));
    assert!(!error.contains("tampered-secret-content"));
    assert!(!tampered_destination.exists());

    fs::write(
        bundle_dir
            .join(TASK_ARTIFACTS_DIR_NAME)
            .join(&entries[0].blob),
        b"original",
    )
    .unwrap();
    let events = bundle_dir.join(TASK_EVENTS_FILE_NAME);
    fs::write(
        &events,
        format!("{}{{", fs::read_to_string(&events).unwrap()),
    )
    .unwrap();
    let jsonl_destination = root.path().join("invalid-jsonl");
    let error = build_publication_snapshot(
        &registry,
        &jsonl_destination,
        metadata(workspace_id),
        &policy(AttachmentPolicyKind::Omit),
        None,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("ORB-00001"));
    assert!(error.contains("events.jsonl"));
    assert!(!jsonl_destination.exists());
}

#[test]
fn invalid_yaml_and_unsupported_bundle_schema_leave_destination_unpublished() {
    for (workspace_id, task_yaml) in [
        ("ws_pub_yaml", "not: [valid"),
        (
            "ws_pub_schema",
            "schema_version: 999\nid: ORB-00001\ntitle: future\n",
        ),
    ] {
        let root = TempDir::new().unwrap();
        let registry = open_registry(root.path());
        let binding = bind(&registry, root.path(), workspace_id);
        let store = bundle_store(&registry, &binding);
        seed(
            &store,
            &registry,
            workspace_id,
            &make_bundle("ORB-00001", "fixture", Vec::new()),
        );
        fs::write(
            store.bundle_path("ORB-00001").unwrap().join("task.yaml"),
            task_yaml,
        )
        .unwrap();
        let destination = root.path().join("unpublished");
        let error = build_publication_snapshot(
            &registry,
            &destination,
            metadata(workspace_id),
            &policy(AttachmentPolicyKind::Fail),
            None,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("ORB-00001"));
        assert!(error.contains("task.yaml"));
        assert!(!destination.exists());
    }
}

/// Attachment deny globs share the policy compiler. A recursive pattern has to
/// reject a path whose segment contains a newline, and leave a non-matching
/// newline path publishable.
#[test]
fn publication_deny_patterns_match_newline_segments() {
    fn publish(workspace_id: &str, path: &str, deny: &str) -> Result<(), String> {
        let root = TempDir::new().unwrap();
        let registry = open_registry(root.path());
        let binding = bind(&registry, root.path(), workspace_id);
        let store = bundle_store(&registry, &binding);
        seed_artifacts(
            &store,
            &registry,
            workspace_id,
            "ORB-00001",
            &[(path, b"n")],
        );
        let mut attachment_policy = policy(AttachmentPolicyKind::Include);
        attachment_policy.deny_patterns = vec![deny.to_string()];
        let destination = root.path().join("published");
        match build_publication_snapshot(
            &registry,
            &destination,
            metadata(workspace_id),
            &attachment_policy,
            Some(&ClearScanner),
        ) {
            Ok(outcome) => {
                assert_eq!(outcome.included_attachment_bytes, 1, "{path:?}");
                let published = read_bundle_at(
                    &destination
                        .join(PUBLICATION_TASKS_DIR_NAME)
                        .join("ORB-00001"),
                )
                .unwrap();
                let paths: Vec<_> = published
                    .artifact_manifest
                    .unwrap()
                    .files
                    .into_iter()
                    .map(|file| file.path)
                    .collect();
                assert_eq!(paths, vec![path.to_string()]);
                Ok(())
            }
            Err(error) => Err(error.to_string()),
        }
    }

    for (workspace_id, path, deny) in [
        ("ws_pub_nl_secret", "secrets/a\nb", "secrets/**"),
        ("ws_pub_nl_sibling", "secrets/ab", "secrets/**"),
        ("ws_pub_nl_leaf", "a\nb/leaf", "**/leaf"),
    ] {
        let error = publish(workspace_id, path, deny).expect_err(path);
        assert!(
            error.contains("deny pattern"),
            "{path:?} against `{deny}`: {error}"
        );
    }

    publish("ws_pub_nl_notes", "notes/a\nb", "secrets/**").expect("non-matching newline path");
}

fn status_event(event_id: &str, from: TaskStatus, to: TaskStatus) -> TaskEventRowV2 {
    TaskEventRowV2 {
        schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
        event_id: event_id.to_string(),
        at: Utc.with_ymd_and_hms(2026, 8, 30, 2, 0, 0).unwrap(),
        by: "codex".to_string(),
        event_type: "status_changed".to_string(),
        note: None,
        from_status: Some(from),
        to_status: Some(to),
    }
}

/// Apply a lifecycle transition the way the task repository does: event append
/// first, envelope republish as the commit point, all under the bundle lock.
/// `between` runs after the append, while the bundle is torn on disk.
fn transition_to_in_progress(
    store: &TaskBundleStoreV2,
    task_id: &str,
    between: impl FnOnce(),
) -> Result<(), OrbitError> {
    store.with_bundle_write_lock(task_id, || {
        let mut envelope = read_bundle_at(&store.bundle_path(task_id)?)?.envelope;
        store.append_event(
            task_id,
            &status_event("EV-0002", TaskStatus::Backlog, TaskStatus::InProgress),
        )?;
        between();
        store.rewrite_document(task_id, TaskDocumentV2::Description, "updated description")?;
        envelope.status = TaskStatus::InProgress;
        envelope.title = "updated title".to_string();
        store.rewrite_envelope(task_id, &envelope)
    })
}

fn published_bundle(destination: &Path, task_id: &str) -> TaskBundleV2 {
    read_bundle_at(&destination.join(PUBLICATION_TASKS_DIR_NAME).join(task_id))
        .expect("read published bundle")
}

/// ORB-13607: publication used to read canonical bundles without the writer's
/// lock, so a transition caught between its event append and envelope publish
/// failed as corruption or published a mixed revision.
#[test]
fn publication_waits_for_in_flight_task_update_and_captures_the_new_bundle() {
    let root = TempDir::new().unwrap();
    let registry = open_registry(root.path());
    let workspace_id = "ws_pub_overlap_update";
    let binding = bind(&registry, root.path(), workspace_id);
    let store = bundle_store(&registry, &binding);
    seed(
        &store,
        &registry,
        workspace_id,
        &make_bundle("ORB-00001", "original title", Vec::new()),
    );

    let (torn_tx, torn_rx) = std::sync::mpsc::sync_channel(0);
    let writer_root = root.path().to_path_buf();
    let writer_binding = binding.clone();
    let writer = std::thread::spawn(move || {
        let registry = open_registry(&writer_root);
        let store = bundle_store(&registry, &writer_binding);
        transition_to_in_progress(&store, "ORB-00001", || {
            torn_tx.send(()).expect("signal torn bundle");
            // Publication starts now and must wait for the commit below; the
            // pause only widens the window an unlocked reader would hit.
            std::thread::sleep(std::time::Duration::from_millis(150));
        })
    });
    torn_rx.recv().expect("writer reached the torn state");

    let destination = root.path().join("snapshot");
    build_publication_snapshot(
        &registry,
        &destination,
        metadata(workspace_id),
        &policy(AttachmentPolicyKind::Fail),
        None,
    )
    .expect("publication must wait for the writer, not report corruption");
    writer
        .join()
        .expect("writer thread")
        .expect("writer commits");

    let published = published_bundle(&destination, "ORB-00001");
    assert_eq!(published, store.read_bundle("ORB-00001").unwrap());
    assert_eq!(published.envelope.status, TaskStatus::InProgress);
    assert_eq!(published.envelope.title, "updated title");
    assert_eq!(published.description, "updated description");
    assert_eq!(published.events.len(), 2);
}

/// Records every scanner input and, on the first call, replaces the task's
/// only artifact and transitions the task — the overlap a publication used to
/// turn into a copy that no longer matched its manifest.
struct ReplacingScanner<'a> {
    store: &'a TaskBundleStoreV2,
    seen: std::cell::RefCell<Vec<Vec<u8>>>,
}

impl AttachmentSensitivityScanner for ReplacingScanner<'_> {
    fn scan(
        &self,
        input: AttachmentScanInput<'_>,
    ) -> Result<AttachmentScanOutcome, AttachmentScanFailure> {
        let first = self.seen.borrow().is_empty();
        self.seen.borrow_mut().push(input.bytes.to_vec());
        if first {
            self.store
                .with_bundle_write_lock(input.task_id, || {
                    let entry =
                        seed_artifact_blob(self.store, input.task_id, input.path, b"new", "codex");
                    self.store.rewrite_artifact_manifest(
                        input.task_id,
                        &ArtifactManifestV2 {
                            schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
                            files: vec![entry],
                        },
                    )
                })
                .expect("replace artifact during scan");
            transition_to_in_progress(self.store, input.task_id, || {})
                .expect("update task during scan");
        }
        Ok(AttachmentScanOutcome::Clear)
    }
}

#[test]
fn artifact_replacement_during_scan_publishes_the_captured_scanned_bytes() {
    let root = TempDir::new().unwrap();
    let registry = open_registry(root.path());
    let workspace_id = "ws_pub_overlap_artifact";
    let binding = bind(&registry, root.path(), workspace_id);
    let store = bundle_store(&registry, &binding);
    let captured = seed_artifacts(
        &store,
        &registry,
        workspace_id,
        "ORB-00001",
        &[("result.txt", b"original")],
    );
    let before = store.read_bundle("ORB-00001").unwrap();

    let scanner = ReplacingScanner {
        store: &store,
        seen: std::cell::RefCell::new(Vec::new()),
    };
    let destination = root.path().join("snapshot");
    let outcome = build_publication_snapshot(
        &registry,
        &destination,
        metadata(workspace_id),
        &policy(AttachmentPolicyKind::Include),
        Some(&scanner),
    )
    .expect("a replacement after capture must not reject the snapshot");
    assert_eq!(outcome.included_attachment_bytes, 8);

    let published = published_bundle(&destination, "ORB-00001");
    assert_eq!(
        published, before,
        "snapshot must be the complete old bundle"
    );
    let manifest = published.artifact_manifest.expect("published manifest");
    assert_eq!(manifest.files, captured);
    let copied = fs::read(
        destination
            .join(PUBLICATION_TASKS_DIR_NAME)
            .join("ORB-00001")
            .join(TASK_ARTIFACTS_DIR_NAME)
            .join(&manifest.files[0].blob),
    )
    .unwrap();
    assert_eq!(copied, b"original");
    assert_eq!(
        format!("{:x}", sha2::Sha256::digest(&copied)),
        manifest.files[0].sha256
    );
    assert_eq!(*scanner.seen.borrow(), vec![copied]);

    let canonical = store.read_bundle("ORB-00001").unwrap();
    assert_eq!(canonical.envelope.status, TaskStatus::InProgress);
    assert_ne!(canonical.artifact_manifest.unwrap().files, captured);
}

#[test]
fn publication_recovers_an_interrupted_write_and_releases_the_bundle_lock() {
    use std::io::Write as _;

    let root = TempDir::new().unwrap();
    let registry = open_registry(root.path());
    let workspace_id = "ws_pub_pending";
    let binding = bind(&registry, root.path(), workspace_id);
    let store = bundle_store(&registry, &binding);
    seed(
        &store,
        &registry,
        workspace_id,
        &make_bundle("ORB-00001", "interrupted", Vec::new()),
    );
    let before = store.read_bundle("ORB-00001").unwrap();
    let bundle_dir = store.bundle_path("ORB-00001").unwrap();

    // A writer that died mid-append: pending record retained, partial JSONL
    // row on disk, envelope never republished.
    store
        .with_bundle_write_lock("ORB-00001", || {
            let pending = PendingWriteGuard::begin(&bundle_dir)?;
            fs::OpenOptions::new()
                .append(true)
                .open(bundle_dir.join(TASK_EVENTS_FILE_NAME))?
                .write_all(b"{\"schema_version\":")?;
            inject_bundle_write_faults(&[BundleWriteFault::DuringCompensation]);
            drop(pending);
            Ok(())
        })
        .expect("leave interrupted write state");
    assert!(bundle_dir.join(PENDING_WRITE_FILE_NAME).is_file());

    let destination = root.path().join("snapshot");
    build_publication_snapshot(
        &registry,
        &destination,
        metadata(workspace_id),
        &policy(AttachmentPolicyKind::Fail),
        None,
    )
    .expect("publication recovers the aborted write");

    assert_eq!(published_bundle(&destination, "ORB-00001"), before);
    assert!(!bundle_dir.join(PENDING_WRITE_FILE_NAME).exists());
    assert_eq!(store.read_bundle("ORB-00001").unwrap(), before);
    transition_to_in_progress(&store, "ORB-00001", || {})
        .expect("publication released the bundle lock");
}
