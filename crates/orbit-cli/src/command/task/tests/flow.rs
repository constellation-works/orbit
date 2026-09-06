//! Sibling tests for `task/flow.rs` (docs/design-patterns/test_layout.md).

use chrono::{DateTime, Duration, TimeZone, Utc};

use orbit_core::TaskStatus;

use crate::command::task::flow::{FlowPoint, StatusChange, TerminalKind, compute_flow, format_net};

fn at(day: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, day, 12, 0, 0)
        .single()
        .expect("fixture timestamp is unambiguous")
}

fn open(created: u32) -> FlowPoint {
    FlowPoint {
        created_at: at(created),
        status_changes: Vec::new(),
    }
}

fn terminal(created: u32, ended: u32, kind: TerminalKind) -> FlowPoint {
    FlowPoint {
        created_at: at(created),
        status_changes: vec![StatusChange {
            at: at(ended),
            status: match kind {
                TerminalKind::Closed => TaskStatus::Done,
                TerminalKind::Dropped => TaskStatus::Rejected,
            },
        }],
    }
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

/// Buckets are [start, end), so a task created exactly on a boundary belongs to
/// the later window only — otherwise a boundary task is counted twice.
#[test]
fn bucket_boundaries_are_half_open() {
    let now = at(21);
    let width = Duration::days(7);
    // at(14) is exactly the boundary between the two 7-day windows.
    let report = compute_flow(&[open(14)], now, width, 2);
    assert_eq!(
        report.buckets[0].filed, 0,
        "boundary belongs to the later window"
    );
    assert_eq!(report.buckets[1].filed, 1);
    assert_eq!(report.filed, 1, "counted once across the whole span");
}

#[test]
fn closed_and_dropped_are_counted_separately() {
    let report = compute_flow(
        &[
            terminal(1, 16, TerminalKind::Closed),
            terminal(1, 16, TerminalKind::Dropped),
            terminal(1, 16, TerminalKind::Dropped),
        ],
        at(21),
        Duration::days(7),
        2,
    );
    assert_eq!(report.closed, 1);
    assert_eq!(report.dropped, 2);
    // Filed before the reported span, so outflow alone drives the net.
    assert_eq!(report.net(), -3);
}

#[test]
fn open_at_end_reflects_the_population_still_open_when_the_window_closed() {
    // Filed day 1, closed day 16: open at the end of the first window, gone by
    // the end of the second.
    let report = compute_flow(
        &[terminal(1, 16, TerminalKind::Closed)],
        at(21),
        Duration::days(7),
        2,
    );
    assert_eq!(report.buckets[0].open_at_end, 1);
    assert_eq!(report.buckets[1].open_at_end, 0);
    assert_eq!(report.open_now, 0);
}

/// A rejected task remains out of the historical population until its later
/// reopen, and its rejected transition is still an outflow event.
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

/// Repeated terminal/reopen cycles retain each historical outflow, but every
/// return to the backlog is matching inflow rather than an artificial drain.
#[test]
fn repeated_reopens_do_not_make_a_stable_open_task_look_draining() {
    let report = compute_flow(
        &[status_history(
            1,
            &[
                (9, TaskStatus::Done),
                (10, TaskStatus::InProgress),
                (15, TaskStatus::Rejected),
                (16, TaskStatus::Backlog),
            ],
        )],
        at(21),
        Duration::days(7),
        2,
    );

    assert_eq!(report.filed, 0);
    assert_eq!(report.reopened, 2);
    assert_eq!(report.closed, 1);
    assert_eq!(report.dropped, 1);
    assert_eq!(report.buckets[0].closed, 1);
    assert_eq!(report.buckets[0].dropped, 0);
    assert_eq!(report.buckets[0].reopened, 1);
    assert_eq!(report.buckets[0].open_at_end, 1);
    assert_eq!(report.buckets[1].closed, 0);
    assert_eq!(report.buckets[1].dropped, 1);
    assert_eq!(report.buckets[1].reopened, 1);
    assert_eq!(report.buckets[1].open_at_end, 1);
    assert_eq!(report.net(), 0);
    assert_eq!(report.open_now, 1);
    assert!(report.verdict().starts_with("flat"), "{}", report.verdict());
}

/// A task that reaches `done` and is later `archived` (routine housekeeping,
/// not a reopen) must contribute exactly one outflow event, not two: the
/// archive is a reclassification of an already-departed task, not a second
/// departure. ORB-11206.
#[test]
fn done_then_archived_contributes_a_single_outflow_event() {
    let report = compute_flow(
        &[status_history(
            1,
            &[(9, TaskStatus::Done), (16, TaskStatus::Archived)],
        )],
        at(21),
        Duration::days(7),
        3,
    );

    // Bucket 0: [day 0, day 7) — the task is filed.
    assert_eq!(report.buckets[0].filed, 1);
    assert_eq!(report.buckets[0].closed, 0);
    assert_eq!(report.buckets[0].dropped, 0);
    // Bucket 1: [day 7, day 14) — the `Done` transition is the one outflow event.
    assert_eq!(report.buckets[1].closed, 1);
    assert_eq!(report.buckets[1].dropped, 0);
    // Bucket 2: [day 14, day 21) — the later `Archived` transition is a
    // relabeling of the same departure, not a second one.
    assert_eq!(report.buckets[2].closed, 0);
    assert_eq!(report.buckets[2].dropped, 0);

    assert_eq!(report.filed, 1);
    assert_eq!(report.reopened, 0);
    assert_eq!(report.closed, 1);
    assert_eq!(report.dropped, 0);
    assert_eq!(report.net(), 0);
    assert_eq!(report.open_now, 0);
    assert!(report.verdict().starts_with("flat"), "{}", report.verdict());
}

#[test]
fn a_task_created_after_a_window_is_not_open_during_it() {
    let report = compute_flow(&[open(20)], at(21), Duration::days(7), 2);
    assert_eq!(report.buckets[0].open_at_end, 0);
    assert_eq!(report.buckets[1].open_at_end, 1);
    assert_eq!(report.open_now, 1);
}

#[test]
fn verdict_names_the_direction_of_the_net() {
    let growing = compute_flow(&[open(15), open(16)], at(21), Duration::days(7), 1);
    assert!(
        growing.verdict().starts_with("growing"),
        "{}",
        growing.verdict()
    );

    let draining = compute_flow(
        &[terminal(1, 16, TerminalKind::Closed)],
        at(21),
        Duration::days(7),
        1,
    );
    assert!(
        draining.verdict().starts_with("draining"),
        "{}",
        draining.verdict()
    );

    // One filed and one closed inside the same window cancel out.
    let flat = compute_flow(
        &[open(16), terminal(1, 16, TerminalKind::Closed)],
        at(21),
        Duration::days(7),
        1,
    );
    assert!(flat.verdict().starts_with("flat"), "{}", flat.verdict());
}

/// Zero equals zero is arithmetically flat and substantively nothing. A filter
/// that matched no tasks must not read as a healthy backlog.
#[test]
fn an_empty_population_reports_no_data_rather_than_flat() {
    let report = compute_flow(&[], at(21), Duration::days(7), 3);
    assert_eq!(report.net(), 0);
    assert!(
        report.verdict().starts_with("no data"),
        "{}",
        report.verdict()
    );
}

/// A population that is entirely open outside the reported windows still has
/// data — the verdict must not fall back to "no data" just because inflow and
/// outflow were both zero in-span.
#[test]
fn open_tasks_outside_the_span_still_count_as_data() {
    let report = compute_flow(&[open(1)], at(21), Duration::days(1), 2);
    assert_eq!(report.filed, 0);
    assert_eq!(report.open_now, 1);
    assert!(report.verdict().starts_with("flat"), "{}", report.verdict());
}

#[test]
fn buckets_are_ordered_oldest_first_and_end_at_now() {
    let now = at(21);
    let report = compute_flow(&[], now, Duration::days(7), 3);
    assert_eq!(report.buckets.len(), 3);
    assert!(report.buckets[0].start < report.buckets[1].start);
    assert_eq!(report.buckets[2].end, now);
    assert_eq!(report.buckets[0].start, now - Duration::days(21));
}

#[test]
fn net_renders_a_leading_sign_only_when_the_backlog_grew() {
    assert_eq!(format_net(3), "+3");
    assert_eq!(format_net(0), "0");
    assert_eq!(format_net(-3), "-3");
}
