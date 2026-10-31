//! Torn-read regression: metadata and cells must come from one snapshot.
//!
//! The `cfg(test)` rendezvous runs after the metadata SELECT while the
//! read transaction is open, and commits a replacement through the writer.
//! A file-backed WAL store is required so that commit can proceed.

use chrono::{TimeZone, Utc};

use super::super::crew_performance_store::{
    clear_crew_performance_snapshot_rendezvous, set_crew_performance_snapshot_rendezvous,
};
use crate::Store;
use crate::contracts::{CrewPerformanceCell, CrewPerformanceSnapshot, CrewPerformanceStoreBackend};

const WORKSPACE: &str = "ws-crew-perf";

fn snapshot(crew: &str, computed_at: chrono::DateTime<Utc>) -> CrewPerformanceSnapshot {
    CrewPerformanceSnapshot {
        workspace_id: WORKSPACE.to_string(),
        computed_at,
        window_days: 30,
        types_hash: "hash".to_string(),
        types_json: "[]".to_string(),
        cells: vec![CrewPerformanceCell {
            crew: crew.to_string(),
            model: "model".to_string(),
            tier: "tier".to_string(),
            task_type: "other".to_string(),
            n: 1,
            raw: 1.0,
            crew_mean: 1.0,
            score: 1.0,
        }],
    }
}

#[test]
fn concurrent_replace_during_snapshot_read_returns_old_metadata_with_old_cells() {
    clear_crew_performance_snapshot_rendezvous();
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open(&dir.path().join("crew.db")).expect("open store");
    let old_at = Utc
        .with_ymd_and_hms(2026, 10, 10, 0, 0, 0)
        .single()
        .expect("old time");
    let new_at = Utc
        .with_ymd_and_hms(2026, 10, 11, 0, 0, 0)
        .single()
        .expect("new time");
    let old = snapshot("old-crew", old_at);
    store
        .replace_crew_performance_snapshot(&old)
        .expect("seed old snapshot");

    let writer = store.clone();
    let replacement = snapshot("new-crew", new_at);
    set_crew_performance_snapshot_rendezvous(move || {
        writer
            .replace_crew_performance_snapshot(&replacement)
            .expect("replace during read");
    });
    let read = store
        .crew_performance_snapshot(WORKSPACE)
        .expect("snapshot read")
        .expect("snapshot present");
    clear_crew_performance_snapshot_rendezvous();
    assert_eq!(
        read, old,
        "old metadata must come back with old cells, never new cells"
    );

    let after = store
        .crew_performance_snapshot(WORKSPACE)
        .expect("snapshot read after")
        .expect("snapshot present after");
    assert_eq!(after.cells[0].crew, "new-crew");
    assert_eq!(after.computed_at, new_at);
}
