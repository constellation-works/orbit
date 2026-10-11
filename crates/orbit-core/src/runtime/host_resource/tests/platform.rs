//! Admitted unit tests: platform-kernel disk reads and combinatorial pure CPU
//! window selection with injected ticks/time/waits. No load generation.
use super::super::platform::disk_percent;
use std::path::Path;

#[test]
fn missing_descendant_reports_its_existing_ancestor_filesystem() {
    let existing = Path::new(env!("CARGO_MANIFEST_DIR"));
    let missing = existing.join("no-such-dir-for-disk-probe/nested");
    assert!(disk_percent(existing).is_some());
    assert!(
        disk_percent(&missing).is_some(),
        "a worktrees directory that does not exist yet must use its nearest existing ancestor"
    );
}

#[test]
fn relative_path_is_refused_instead_of_resolved_against_the_working_directory() {
    assert!(
        disk_percent(Path::new("crates/orbit-core")).is_none(),
        "a relative probe path must be refused, never resolved against the working directory"
    );
}

fn ticks(millis: i64, counters: [u32; 4]) -> super::super::platform::CpuTickSample {
    super::super::platform::CpuTickSample {
        sampled_at: chrono::DateTime::from_timestamp_millis(1_700_000_000_000 + millis).unwrap(),
        ticks: counters,
    }
}

#[test]
fn cpu_window_reuses_advancing_or_repeated_ticks_without_refreshing_the_baseline() {
    use super::super::HostCpuTickHistory;
    let mut history = HostCpuTickHistory {
        previous: Some(ticks(0, [10, 10, 60, 10])),
        latest: Some(ticks(1100, [20, 20, 120, 20])),
    };
    // Repeated counters use the previous end of the last real window, even
    // across multiple rapid processes. That end must still expire at 15s.
    for at in [1200, 1400, 14999] {
        assert_eq!(
            history.measure(
                || Some(ticks(at, [20, 20, 120, 20])),
                |_| panic!("fresh real window must not wait")
            ),
            Some(100.0 * 30.0 / 90.0)
        );
        assert_eq!(
            history.previous.as_ref().unwrap().sampled_at,
            ticks(0, [0; 4]).sampled_at
        );
        assert_eq!(
            history.latest.as_ref().unwrap().sampled_at,
            ticks(1100, [0; 4]).sampled_at
        );
    }
    assert_eq!(
        history.measure(
            || Some(ticks(15000, [30, 30, 180, 30])),
            |_| panic!("advancing latest ticks must not wait")
        ),
        Some(100.0 * 30.0 / 90.0)
    );
    assert_eq!(
        history.previous.as_ref().unwrap().sampled_at,
        ticks(1100, [0; 4]).sampled_at
    );
}

#[test]
fn cpu_window_missing_stale_future_or_identical_baselines_take_a_real_window() {
    use super::super::{HostCpuTickHistory, platform::CPU_BASELINE_WINDOW};
    // Age endpoints, missing history, a future clock and an identical cached
    // observation all exercise the actual sampling decision, not just parsing.
    for (baseline_at, counters) in [
        (None, [1; 4]),
        (Some(-15001), [1; 4]),
        (Some(1), [1; 4]),
        (Some(-100), [10, 10, 60, 10]),
    ] {
        let mut history = HostCpuTickHistory {
            previous: None,
            latest: baseline_at.map(|at| ticks(at, counters)),
        };
        let mut reads = [
            Some(ticks(0, [10, 10, 60, 10])),
            Some(ticks(1100, [20, 20, 120, 20])),
        ]
        .into_iter();
        let mut waits = Vec::new();
        assert_eq!(
            history.measure(|| reads.next().unwrap(), |duration| waits.push(duration)),
            Some(100.0 * 30.0 / 90.0)
        );
        assert_eq!(waits, vec![CPU_BASELINE_WINDOW]);
        assert_eq!(
            history.previous.as_ref().unwrap().sampled_at,
            ticks(0, [0; 4]).sampled_at
        );
    }
    // A fresh latest end cannot make the older end usable once it has expired.
    let mut history = HostCpuTickHistory {
        previous: Some(ticks(-15001, [1; 4])),
        latest: Some(ticks(-100, [10, 10, 60, 10])),
    };
    let mut reads = [
        Some(ticks(0, [10, 10, 60, 10])),
        Some(ticks(1100, [20, 20, 120, 20])),
    ]
    .into_iter();
    let mut waits = 0;
    assert!(
        history
            .measure(|| reads.next().unwrap(), |_| waits += 1)
            .is_some()
    );
    assert_eq!(waits, 1);
}

#[test]
fn cpu_window_native_failures_clock_reversal_and_stalls_remain_unknown() {
    use super::super::HostCpuTickHistory;
    for second in [
        None,
        Some(ticks(-1, [20; 4])),
        Some(ticks(15001, [20; 4])),
        Some(ticks(1100, [10; 4])),
    ] {
        let mut history = HostCpuTickHistory::default();
        let mut reads = [Some(ticks(0, [10; 4])), second].into_iter();
        assert_eq!(history.measure(|| reads.next().unwrap(), |_| {}), None);
    }
    let mut history = HostCpuTickHistory {
        previous: None,
        latest: Some(ticks(0, [10; 4])),
    };
    assert_eq!(
        history.measure(|| None, |_| panic!("failed read must not wait")),
        None
    );
    assert!(history.latest.is_none());
}

#[test]
fn cpu_window_handles_tick_wrap_and_the_freshness_boundary() {
    use super::super::HostCpuTickHistory;
    for age in [1, 15000] {
        let mut history = HostCpuTickHistory {
            previous: None,
            latest: Some(ticks(-age, [u32::MAX - 9; 4])),
        };
        assert_eq!(
            history.measure(
                || Some(ticks(0, [10; 4])),
                |_| panic!("fresh advancing counters must not wait")
            ),
            Some(75.0)
        );
    }
}
