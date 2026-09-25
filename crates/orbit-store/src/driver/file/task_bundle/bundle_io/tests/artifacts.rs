use std::fs;

use chrono::{TimeZone, Utc};
use orbit_common::OrbitError;
use orbit_common::fs::io::atomic_write_text;
use orbit_types::task::{
    ArtifactManifestFileV2, ArtifactManifestV2, TASK_ARTIFACT_FILES_DIR_NAME,
    TASK_ARTIFACT_SCHEMA_VERSION, TASK_ARTIFACTS_DIR_NAME,
};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

use super::super::{read_bundle_at, read_bundle_lightweight_at, take_artifact_payload_reads};
use crate::repository::task::tests::test_support::{bundle_store, sample_bundle};

#[test]
fn read_bundle_rejects_manifest_entry_with_missing_artifact_file() {
    let temp = TempDir::new().expect("tempdir");
    let store = bundle_store(&temp);
    store
        .create_bundle(&sample_bundle("ORB-00000"))
        .expect("create bundle");
    let now = Utc.with_ymd_and_hms(2026, 5, 11, 12, 0, 0).unwrap();
    let bundle_dir = store.bundle_path("ORB-00000").expect("bundle path");
    let blob = format!("{TASK_ARTIFACT_FILES_DIR_NAME}/result.txt");
    let blob_path = bundle_dir.join(TASK_ARTIFACTS_DIR_NAME).join(&blob);
    atomic_write_text(&blob_path, "hello").expect("write artifact blob");
    let manifest = ArtifactManifestV2 {
        schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
        files: vec![ArtifactManifestFileV2 {
            origin: None,
            path: "result.txt".to_string(),
            blob: blob.clone(),
            sha256: format!("{:x}", Sha256::digest(b"hello")),
            media_type: "text/plain".to_string(),
            size_bytes: 5,
            created_by: "codex:gpt-5.5".to_string(),
            created_at: now,
        }],
    };
    store
        .rewrite_artifact_manifest("ORB-00000", &manifest)
        .expect("write manifest");
    fs::remove_file(blob_path).expect("remove artifact blob");

    assert!(matches!(
        store.read_bundle("ORB-00000"),
        Err(OrbitError::TaskBundleCorrupt {
            task_id,
            reason,
            ..
        }) if task_id == "ORB-00000" && reason.contains("missing file")
    ));
}

#[test]
fn lightweight_read_skips_tampered_artifact_bytes_that_strict_read_rejects() {
    let temp = TempDir::new().expect("tempdir");
    let store = bundle_store(&temp);
    store
        .create_bundle(&sample_bundle("ORB-00000"))
        .expect("create bundle");
    let now = Utc.with_ymd_and_hms(2026, 5, 11, 12, 0, 0).unwrap();
    let bundle_dir = store.bundle_path("ORB-00000").expect("bundle path");
    let blob = format!("{TASK_ARTIFACT_FILES_DIR_NAME}/result.txt");
    let blob_path = bundle_dir.join(TASK_ARTIFACTS_DIR_NAME).join(&blob);
    atomic_write_text(&blob_path, "hello").expect("write artifact blob");
    let manifest = ArtifactManifestV2 {
        schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
        files: vec![ArtifactManifestFileV2 {
            origin: None,
            path: "result.txt".to_string(),
            blob: blob.clone(),
            sha256: format!("{:x}", Sha256::digest(b"hello")),
            media_type: "text/plain".to_string(),
            size_bytes: 5,
            created_by: "codex:gpt-5.5".to_string(),
            created_at: now,
        }],
    };
    store
        .rewrite_artifact_manifest("ORB-00000", &manifest)
        .expect("write manifest");
    atomic_write_text(&blob_path, "wrong").expect("tamper artifact blob");
    let _ = take_artifact_payload_reads();

    let light = read_bundle_lightweight_at(&bundle_dir).expect("lightweight read");
    assert_eq!(take_artifact_payload_reads(), 0);
    assert_eq!(
        light
            .artifact_manifest
            .as_ref()
            .map(|manifest| manifest.files[0].path.as_str()),
        Some("result.txt")
    );

    assert!(matches!(
        read_bundle_at(&bundle_dir),
        Err(OrbitError::TaskBundleCorrupt {
            task_id,
            reason,
            ..
        }) if task_id == "ORB-00000" && reason.contains("sha256 mismatch")
    ));
    assert_eq!(take_artifact_payload_reads(), 1);
}
