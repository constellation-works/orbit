//! Deterministic interleaving of a delayed audit insert between the tail
//! count and the page read.
//!
//! Admitted under boundary-first criterion 2. The HTTP handler cannot pause
//! between those two statements, so this test uses the store's `cfg(test)`
//! rendezvous, which runs while one read snapshot is open. A file-backed WAL
//! store is required: the hook inserts through the writer connection, and an
//! in-memory store's reader is that same connection.

use chrono::{DateTime, Duration, TimeZone, Utc};

use crate::Store;
use crate::contracts::{
    V2AuditEventFilter, V2AuditEventInsertParams, V2AuditEventTailPage, V2AuditStoreBackend,
};

const WORKSPACE: &str = "ws-tail";

/// A delayed insert between the counted tail window and the page read must
/// not change page ranks relative to the reported total, and must not drop
/// the newest event already in that snapshot.
#[test]
fn delayed_insert_during_tail_read_keeps_snapshot_ranks_and_the_newest_event() {
    let _guard = RendezvousGuard::idle();
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open(&dir.path().join("audit.db")).expect("open store");
    let journal = store
        .with_read_connection(|conn| {
            conn.query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0))
                .map_err(|error| orbit_common::OrbitError::Store(error.to_string()))
        })
        .expect("journal mode");
    assert_eq!(
        journal.to_ascii_lowercase(),
        "wal",
        "the rendezvous inserts through the writer while a reader snapshot is open"
    );

    let empty = tail(&store, "run-empty", 100, 0);
    assert_eq!(empty.total, 0);
    assert!(empty.events.is_empty());

    // Insert sorts between events 0 and 1. A stale oldest-first offset of 150
    // would then return events 149-248 and drop event 249.
    assert_snapshot(
        &store,
        "run-before-window",
        0,
        &expected_page("run-before-window"),
    );
    // Insert sorts inside the tail page. A split newest-first read would admit
    // it and drop event 150 while still reporting total 250.
    let mut inside = expected_page("run-inside-window");
    inside.remove(0);
    inside.insert(0, "run-inside-window-delayed".to_string());
    assert_snapshot(&store, "run-inside-window", 150, &inside);
}

fn assert_snapshot(store: &Store, run_id: &str, between: i64, follow_up_ids: &[String]) {
    let base = base_time();
    seed(store, run_id, base);
    let left = (base + Duration::seconds(between)).to_rfc3339();
    let delayed_at = base + Duration::seconds(between) + Duration::microseconds(500);
    let delayed = delayed_at.to_rfc3339();
    let right = (base + Duration::seconds(between + 1)).to_rfc3339();
    assert!(
        left.as_str() < delayed.as_str() && delayed.as_str() < right.as_str(),
        "stored timestamp text must sort between event {between} and event {}: {left} < {delayed} < {right}",
        between + 1
    );

    let before = tail(store, run_id, 100, 0);
    assert_eq!(before.total, 250);
    assert_eq!(page_ids(&before), expected_page(run_id));
    let beyond = tail(store, run_id, 100, 999);
    assert_eq!(
        beyond.total, 250,
        "an out-of-range tail still reports the snapshot total"
    );
    assert!(beyond.events.is_empty());

    let writer = store.clone();
    let delayed_id = format!("{run_id}-delayed");
    let run = run_id.to_string();
    let _armed = RendezvousGuard::arm(move || {
        insert(&writer, &run, &delayed_id, delayed_at);
    });
    let during = tail(store, run_id, 100, 0);
    assert_eq!(
        during.total, 250,
        "the reported total stays the count from the open snapshot"
    );
    assert_eq!(
        page_ids(&during),
        expected_page(run_id),
        "a delayed insert during the tail read must keep events 150-249, including the newest event already present at count time"
    );
    assert!(
        !page_ids(&during).iter().any(|id| id.ends_with("-delayed")),
        "the snapshot page must not contain the row committed after the count"
    );

    assert_eq!(count(store, run_id), 251, "the delayed insert did commit");
    let after = tail(store, run_id, 100, 0);
    assert_eq!(after.total, 251);
    assert_eq!(
        page_ids(&after),
        follow_up_ids,
        "a later read must observe the committed delayed row"
    );
    let ordered = list_oldest(store, run_id);
    let delayed_at_index = ordered
        .iter()
        .position(|id| id.ends_with("-delayed"))
        .expect("delayed row is stored");
    let left_index = ordered
        .iter()
        .position(|id| id == &format!("{run_id}-{between}"))
        .expect("left neighbor");
    assert_eq!(
        delayed_at_index,
        left_index + 1,
        "the delayed timestamp sorts immediately after event {between}"
    );
}

fn base_time() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 10, 0, 0, 0)
        .single()
        .expect("base time")
}

fn seed(store: &Store, run_id: &str, base: DateTime<Utc>) {
    for index in 0..250 {
        insert(
            store,
            run_id,
            &format!("{run_id}-{index}"),
            base + Duration::seconds(index),
        );
    }
}

fn insert(store: &Store, run_id: &str, event_id: &str, ts: DateTime<Utc>) {
    store
        .insert_v2_audit_event(&V2AuditEventInsertParams {
            workspace_id: WORKSPACE.to_string(),
            event_id: event_id.to_string(),
            source: "v2_envelope".to_string(),
            schema_version: 1,
            event_type: "activity_started".to_string(),
            ts,
            run_id: run_id.to_string(),
            agent_identity: "fixture".to_string(),
            parent_event_id: None,
            workspace_path: None,
            payload_json: "{}".to_string(),
        })
        .expect("insert audit event");
}

fn filter(run_id: &str, limit: usize, offset: usize) -> V2AuditEventFilter {
    V2AuditEventFilter {
        workspace_id: WORKSPACE.to_string(),
        run_id: Some(run_id.to_string()),
        source: Some("v2_envelope".to_string()),
        limit: Some(limit),
        offset: Some(offset),
        ..Default::default()
    }
}

fn tail(store: &Store, run_id: &str, limit: usize, offset: usize) -> V2AuditEventTailPage {
    V2AuditStoreBackend::list_v2_audit_event_tail(store, &filter(run_id, limit, offset))
        .expect("tail page")
}

fn count(store: &Store, run_id: &str) -> i64 {
    V2AuditStoreBackend::count_v2_audit_events(store, &filter(run_id, 100, 0)).expect("count")
}

fn list_oldest(store: &Store, run_id: &str) -> Vec<String> {
    let mut query = filter(run_id, 300, 0);
    query.oldest_first = true;
    store
        .list_v2_audit_events(&query)
        .expect("list")
        .into_iter()
        .map(|row| row.event_id)
        .collect()
}

fn expected_page(run_id: &str) -> Vec<String> {
    (150..250)
        .map(|index| format!("{run_id}-{index}"))
        .collect()
}

fn page_ids(page: &V2AuditEventTailPage) -> Vec<String> {
    page.events.iter().map(|row| row.event_id.clone()).collect()
}

struct RendezvousGuard;

impl RendezvousGuard {
    fn idle() -> Self {
        super::clear_tail_snapshot_rendezvous();
        Self
    }

    fn arm(hook: impl FnOnce() + 'static) -> Self {
        super::set_tail_snapshot_rendezvous(hook);
        Self
    }
}

impl Drop for RendezvousGuard {
    fn drop(&mut self) {
        super::clear_tail_snapshot_rendezvous();
    }
}
