//! Sibling tests for `task/flow.rs` (docs/design-patterns/test_layout.md).

use chrono::{DateTime, Duration, TimeZone, Utc};

use orbit_core::TaskStatus;

use crate::command::task::flow::{FlowPoint, StatusChange, compute_flow};

fn at(day: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, day, 12, 0, 0)
        .single()
        .expect("fixture timestamp is unambiguous")
}

fn status_history(created: u32, changes: &[(u32, TaskStatus)]) -> FlowPoint {
    FlowPoint {
        created_at: at(created),
        status_changes: changes
            .iter()
            .map(|(day, status)| StatusChange {
                at: at(*day),
                status: *status,
            })
            .collect(),
    }
}

#[test]
fn rejection_then_reopen_preserves_dropped_event_and_historical_boundaries() {
    let report = compute_flow(
        &[status_history(
            1,
            &[(10, TaskStatus::Rejected), (16, TaskStatus::Backlog)],
        )],
        at(21),
        Duration::days(7),
        2,
    );

    assert_eq!(report.buckets[0].dropped, 1);
    assert_eq!(report.buckets[0].reopened, 0);
    assert_eq!(report.buckets[0].open_at_end, 0);
    assert_eq!(report.buckets[1].dropped, 0);
    assert_eq!(report.buckets[1].reopened, 1);
    assert_eq!(report.buckets[1].open_at_end, 1);
    assert_eq!(report.filed, 0);
    assert_eq!(report.reopened, 1);
    assert_eq!(report.dropped, 1);
    assert_eq!(report.net(), 0);
    assert_eq!(report.open_now, 1);
    assert!(report.verdict().starts_with("flat"), "{}", report.verdict());
}
