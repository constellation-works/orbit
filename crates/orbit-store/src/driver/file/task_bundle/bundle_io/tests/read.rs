use std::fs;

use chrono::{TimeZone, Utc};
use orbit_common::{NotFoundKind, OrbitError};
use orbit_types::task::{
    TASK_ARTIFACT_SCHEMA_VERSION, TASK_ENVELOPE_FILE_NAME, TaskEventRowV2, TaskStatus,
};
use tempfile::TempDir;

use super::super::read_bundle_at;
use crate::repository::task::tests::test_support::{bundle_store, sample_bundle};

#[test]
fn read_bundle_rejects_directory_name_that_differs_from_task_id() {
    let temp = TempDir::new().expect("tempdir");
    let store = bundle_store(&temp);
    let created = store
        .create_bundle(&sample_bundle("ORB-00000"))
        .expect("create bundle");
    let renamed = created.binding.canonical_path.with_file_name("ORB-00009");
    fs::rename(&created.binding.canonical_path, &renamed).expect("rename bundle");

    assert!(matches!(
        read_bundle_at(&renamed),
        Err(OrbitError::TaskBundleCorrupt {
            task_id,
            reason,
            ..
        }) if task_id == "ORB-00009" && reason.contains("contains task id ORB-00000")
    ));
}

#[test]
fn read_bundle_reports_missing_envelope_as_task_not_found() {
    let temp = TempDir::new().expect("tempdir");
    let store = bundle_store(&temp);
    let created = store
        .create_bundle(&sample_bundle("ORB-00000"))
        .expect("create bundle");
    fs::remove_file(created.binding.canonical_path.join(TASK_ENVELOPE_FILE_NAME))
        .expect("remove envelope");

    assert!(matches!(
        store.read_bundle("ORB-00000"),
        Err(OrbitError::NotFound {
            kind: NotFoundKind::Task,
            id: task_id,
        }) if task_id == "ORB-00000"
    ));
}

#[test]
fn read_bundle_rejects_event_status_newer_than_envelope_status() {
    let temp = TempDir::new().expect("tempdir");
    let store = bundle_store(&temp);
    store
        .create_bundle(&sample_bundle("ORB-00000"))
        .expect("create bundle");
    let mut envelope = sample_bundle("ORB-00000").envelope;
    envelope.status = TaskStatus::InProgress;
    store
        .rewrite_envelope("ORB-00000", &envelope)
        .expect("rewrite mismatched envelope");

    assert!(matches!(
        store.read_bundle("ORB-00000"),
        Err(OrbitError::TaskBundleCorrupt {
            task_id,
            reason,
            ..
        }) if task_id == "ORB-00000" && reason.contains("event log status")
    ));
}

#[test]
fn read_bundle_rejects_a_status_event_without_pending_write_evidence() {
    let temp = TempDir::new().expect("tempdir");
    let store = bundle_store(&temp);
    store
        .create_bundle(&sample_bundle("ORB-00000"))
        .expect("create bundle");
    store
        .append_event(
            "ORB-00000",
            &TaskEventRowV2 {
                schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
                event_id: "EV-0002".to_string(),
                at: Utc.with_ymd_and_hms(2026, 5, 11, 13, 0, 0).unwrap(),
                by: "codex:gpt-5.5".to_string(),
                event_type: "status_changed".to_string(),
                note: None,
                from_status: Some(TaskStatus::Backlog),
                to_status: Some(TaskStatus::InProgress),
            },
        )
        .expect("append event without pending record");

    assert!(matches!(
        store.read_bundle("ORB-00000"),
        Err(OrbitError::TaskBundleCorrupt {
            task_id,
            reason,
            ..
        }) if task_id == "ORB-00000" && reason.contains("event log status")
    ));
}
