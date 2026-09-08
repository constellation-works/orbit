use std::fs;
use std::sync::{Arc, Barrier};
use std::thread;

use chrono::{TimeZone, Utc};
use orbit_common::fs::io::atomic_write_text;
use orbit_common::{NotFoundKind, OrbitError};
use orbit_types::task::{
    ArtifactManifestFileV2, ArtifactManifestV2, TASK_ARTIFACT_FILES_DIR_NAME,
    TASK_ARTIFACT_SCHEMA_VERSION, TASK_ARTIFACTS_DIR_NAME, TASK_ENVELOPE_FILE_NAME,
    TASK_EVENTS_FILE_NAME, TaskEventRowV2, TaskStatus,
};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

use super::{
    append_jsonl_row, read_bundle_at, read_bundle_lightweight_at, read_task_events,
    take_artifact_payload_reads, write_bundle_atomically,
};
use crate::repository::task::tests::test_support::{bundle_store, sample_bundle};

#[test]
fn write_and_read_bundle_round_trips_v2_shape() {
    let temp = TempDir::new().expect("tempdir");
    let store = bundle_store(&temp);
    let bundle = sample_bundle("ORB-00000");

    let created = store.create_bundle(&bundle).expect("create bundle");
    assert_eq!(created.binding.task_id, "ORB-00000");

    let read = store.read_bundle("ORB-00000").expect("read bundle");
    assert_eq!(read.envelope, bundle.envelope);
    assert_eq!(read.description, bundle.description);
    assert_eq!(read.acceptance, bundle.acceptance);
    assert_eq!(read.plan, bundle.plan);
    assert_eq!(read.events, bundle.events);
    assert_eq!(read.comments, bundle.comments);
    assert!(
        created
            .binding
            .canonical_path
            .join(TASK_ENVELOPE_FILE_NAME)
            .is_file()
    );
    assert!(
        created
            .binding
            .canonical_path
            .join(TASK_ARTIFACTS_DIR_NAME)
            .join(TASK_ARTIFACT_FILES_DIR_NAME)
            .is_dir()
    );
}

#[test]
fn interrupted_bundle_publish_never_exposes_partial_final_directory() {
    let temp = TempDir::new().expect("tempdir");
    let store = bundle_store(&temp);
    let bundle = sample_bundle("ORB-00000");
    let bundle_dir = store.bundle_path("ORB-00000").expect("bundle path");

    let error = write_bundle_atomically(&bundle_dir, &bundle, None, |staging, final_path| {
        assert!(
            !final_path.exists(),
            "final path must remain absent while staging"
        );
        assert!(staging.join(TASK_ENVELOPE_FILE_NAME).is_file());
        assert!(staging.join(TASK_EVENTS_FILE_NAME).is_file());
        assert!(
            staging
                .join(TASK_ARTIFACTS_DIR_NAME)
                .join(TASK_ARTIFACT_FILES_DIR_NAME)
                .is_dir()
        );
        Err(std::io::Error::new(
            std::io::ErrorKind::Interrupted,
            "simulated interruption before rename",
        ))
    })
    .expect_err("interrupted publish must fail");

    assert!(matches!(error, OrbitError::Io(message) if message.contains("simulated interruption")));
    assert!(
        !bundle_dir.exists(),
        "an interrupted writer must not expose the canonical bundle path"
    );
    let parent = bundle_dir.parent().expect("bundle parent");
    let staging_entries = fs::read_dir(parent)
        .expect("read bundle parent")
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".staging"))
        .collect::<Vec<_>>();
    assert!(
        staging_entries.is_empty(),
        "handled failures clean their private staging directories"
    );
}

#[test]
fn erofs_bundle_publish_names_path_and_hints_sandbox() {
    let temp = TempDir::new().expect("tempdir");
    let store = bundle_store(&temp);
    let bundle = sample_bundle("ORB-00000");
    let bundle_dir = store.bundle_path("ORB-00000").expect("bundle path");

    let error = write_bundle_atomically(&bundle_dir, &bundle, None, |_staging, _final_path| {
        Err(std::io::Error::new(
            std::io::ErrorKind::ReadOnlyFilesystem,
            "Read-only file system",
        ))
    })
    .expect_err("EROFS publish must fail");

    match error {
        OrbitError::Io(message) => {
            assert!(
                message.contains(&bundle_dir.display().to_string()),
                "expected path in `{message}`"
            );
            assert!(
                message.contains("is not writable"),
                "expected writable attribution in `{message}`"
            );
            assert!(
                message.contains("sandbox or environment"),
                "expected sandbox/environment hint in `{message}`"
            );
            assert!(
                message.contains("not an Orbit store defect"),
                "expected store-defect negation in `{message}`"
            );
        }
        other => panic!("expected Io, got {other}"),
    }
    assert!(
        !bundle_dir.exists(),
        "an EROFS writer must not expose the canonical bundle path"
    );
}

#[test]
fn append_jsonl_repairs_corrupt_tail_only() {
    let temp = TempDir::new().expect("tempdir");
    let store = bundle_store(&temp);
    let bundle = sample_bundle("ORB-00000");
    store.create_bundle(&bundle).expect("create bundle");
    let events_path = store
        .bundle_path("ORB-00000")
        .expect("bundle path")
        .join(TASK_EVENTS_FILE_NAME);
    fs::write(&events_path, "{\"schema_version\":1,\"event_id\":\"EV-0001\",\"at\":\"2026-05-11T12:00:00Z\",\"by\":\"codex:gpt-5.5\",\"type\":\"created\",\"to_status\":\"backlog\"}\n{\"schema_version\"")
        .expect("write corrupt tail");

    store
        .append_event(
            "ORB-00000",
            &TaskEventRowV2 {
                schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
                event_id: "EV-0002".to_string(),
                at: Utc.with_ymd_and_hms(2026, 5, 11, 13, 0, 0).unwrap(),
                by: "codex:gpt-5.5".to_string(),
                event_type: "updated".to_string(),
                note: None,
                from_status: None,
                to_status: None,
            },
        )
        .expect("append event");

    let events = read_task_events(&events_path).expect("read events");
    assert_eq!(
        events
            .iter()
            .map(|event| event.event_id.as_str())
            .collect::<Vec<_>>(),
        vec!["EV-0001", "EV-0002"]
    );
}

#[test]
fn append_jsonl_repairs_trailing_newline_corrupt_tail() {
    let temp = TempDir::new().expect("tempdir");
    let store = bundle_store(&temp);
    store
        .create_bundle(&sample_bundle("ORB-00000"))
        .expect("create bundle");
    let events_path = store
        .bundle_path("ORB-00000")
        .expect("bundle path")
        .join(TASK_EVENTS_FILE_NAME);
    fs::write(&events_path, "{\"schema_version\":1,\"event_id\":\"EV-0001\",\"at\":\"2026-05-11T12:00:00Z\",\"by\":\"codex:gpt-5.5\",\"type\":\"created\",\"to_status\":\"backlog\"}\nnot-json\n")
        .expect("write corrupt tail");

    store
        .append_event(
            "ORB-00000",
            &TaskEventRowV2 {
                schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
                event_id: "EV-0002".to_string(),
                at: Utc.with_ymd_and_hms(2026, 5, 11, 13, 0, 0).unwrap(),
                by: "codex:gpt-5.5".to_string(),
                event_type: "updated".to_string(),
                note: None,
                from_status: None,
                to_status: None,
            },
        )
        .expect("append event");

    let events = read_task_events(&events_path).expect("read events");
    assert_eq!(
        events
            .iter()
            .map(|event| event.event_id.as_str())
            .collect::<Vec<_>>(),
        vec!["EV-0001", "EV-0002"]
    );
}

#[test]
fn append_jsonl_serializes_concurrent_writers() {
    let temp = TempDir::new().expect("tempdir");
    let path = Arc::new(temp.path().join("events.jsonl"));
    let barrier = Arc::new(Barrier::new(8));
    let now = Utc.with_ymd_and_hms(2026, 5, 11, 13, 0, 0).unwrap();
    let handles = (0..8)
        .map(|index| {
            let path = Arc::clone(&path);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                append_jsonl_row(
                    &path,
                    &TaskEventRowV2 {
                        schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
                        event_id: format!("EV-{index:04}"),
                        at: now,
                        by: "codex:gpt-5.5".to_string(),
                        event_type: "updated".to_string(),
                        note: None,
                        from_status: None,
                        to_status: None,
                    },
                )
                .expect("append event");
            })
        })
        .collect::<Vec<_>>();

    for handle in handles {
        handle.join().expect("join writer");
    }

    let mut ids = read_task_events(&path)
        .expect("read events")
        .into_iter()
        .map(|event| event.event_id)
        .collect::<Vec<_>>();
    ids.sort();
    assert_eq!(
        ids,
        vec![
            "EV-0000", "EV-0001", "EV-0002", "EV-0003", "EV-0004", "EV-0005", "EV-0006", "EV-0007"
        ]
    );
}

#[test]
fn read_jsonl_rejects_corruption_before_tail() {
    let temp = TempDir::new().expect("tempdir");
    let path = temp.path().join("events.jsonl");
    fs::write(
        &path,
        "{\"schema_version\":1,\"event_id\":\"EV-0001\",\"at\":\"2026-05-11T12:00:00Z\",\"by\":\"codex:gpt-5.5\",\"type\":\"created\",\"to_status\":\"backlog\"}\nnot-json\n{\"schema_version\":1,\"event_id\":\"EV-0002\",\"at\":\"2026-05-11T13:00:00Z\",\"by\":\"codex:gpt-5.5\",\"type\":\"updated\"}\n",
    )
    .expect("write invalid middle");

    assert!(matches!(
        read_task_events(&path),
        Err(OrbitError::Store(message)) if message.contains("before tail")
    ));
}

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

/// `write_yaml_atomic_with` is bound to durable `atomic_write_text`, so both
/// bundle create and envelope rewrite fsync the `task.yaml` temp file before
/// rename. When `strace` is available this asserts the syscall order; a
/// sandbox denial is not a product failure.
#[test]
fn task_yaml_temp_is_fsynced_before_rename_on_create_and_rewrite() {
    exercise_task_yaml_create_and_rewrite();

    #[cfg(target_os = "linux")]
    trace_task_yaml_fsync_before_rename();
}

fn exercise_task_yaml_create_and_rewrite() {
    let temp = TempDir::new().expect("tempdir");
    let store = bundle_store(&temp);
    store
        .create_bundle(&sample_bundle("ORB-00000"))
        .expect("create bundle");
    let mut envelope = sample_bundle("ORB-00000").envelope;
    envelope.title = "Rewritten".to_string();
    store
        .rewrite_envelope("ORB-00000", &envelope)
        .expect("rewrite envelope");
    let read = store.read_bundle("ORB-00000").expect("read");
    assert_eq!(read.envelope.title, "Rewritten");
}

#[cfg(target_os = "linux")]
fn trace_task_yaml_fsync_before_rename() {
    use std::process::Command;

    if std::env::var_os("ORB_TASK_YAML_FSYNC_PROBE").is_some() {
        return;
    }
    let Ok(version) = Command::new("strace").arg("-V").output() else {
        return;
    };
    if !version.status.success() {
        return;
    }
    let log = tempfile::NamedTempFile::new().expect("strace log");
    let Some(test_name) = std::thread::current().name().map(ToOwned::to_owned) else {
        return;
    };
    let output = Command::new("strace")
        .args([
            "-f",
            "-y",
            "-e",
            "trace=fsync,fdatasync,rename,renameat,renameat2",
            "-o",
        ])
        .arg(log.path())
        .arg(std::env::current_exe().expect("test exe"))
        .args(["--exact", &test_name])
        .env("ORB_TASK_YAML_FSYNC_PROBE", "1")
        .env("RUST_TEST_THREADS", "1")
        .output();
    let output = match output {
        Ok(output) => output,
        Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => return,
        Err(_) => return,
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("Operation not permitted")
            || stderr.contains("Permission denied")
            || stderr.contains("not permitted")
        {
            return;
        }
        // Probe ran the test body; parse the log even if the harness exit is noisy.
    }
    let trace = fs::read_to_string(log.path()).unwrap_or_default();
    if trace.is_empty() {
        return;
    }
    assert_task_yaml_tmp_fsynced_before_rename(&trace);
}

#[cfg(target_os = "linux")]
fn assert_task_yaml_tmp_fsynced_before_rename(trace: &str) {
    let mut last_rename_line = 0usize;
    let mut last_fsync_line = 0usize;
    let mut renames = 0usize;
    for (index, line) in trace.lines().enumerate() {
        let number = index + 1;
        if (line.contains("fsync") || line.contains("fdatasync")) && !line.contains("unfinished") {
            last_fsync_line = number;
        }
        if is_task_yaml_tmp_rename(line) {
            assert!(
                last_fsync_line > last_rename_line,
                "task.yaml temp rename without a preceding fsync (rename line {number}): {line}\n{trace}"
            );
            last_rename_line = number;
            renames += 1;
        }
    }
    assert!(
        renames >= 2,
        "expected fsynced create and rewrite renames of task.yaml, found {renames}:\n{trace}"
    );
}

#[cfg(target_os = "linux")]
fn is_task_yaml_tmp_rename(line: &str) -> bool {
    let is_rename =
        line.contains("rename(") || line.contains("renameat(") || line.contains("renameat2(");
    is_rename && line.contains(".task.yaml.") && line.contains(".tmp") && line.contains("task.yaml")
}
