use std::fs;
use std::sync::{Arc, Barrier};
use std::thread;

use chrono::{TimeZone, Utc};
use orbit_common::OrbitError;
use orbit_types::task::{TASK_ARTIFACT_SCHEMA_VERSION, TASK_EVENTS_FILE_NAME, TaskEventRowV2};
use tempfile::TempDir;

use super::super::append_jsonl_row;
use super::super::jsonl::read_task_events;
use crate::repository::task::tests::test_support::{bundle_store, sample_bundle};

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
