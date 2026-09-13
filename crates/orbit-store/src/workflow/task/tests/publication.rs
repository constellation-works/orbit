use std::collections::BTreeMap;

use orbit_types::task::TASK_EVENTS_FILE_NAME;

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
