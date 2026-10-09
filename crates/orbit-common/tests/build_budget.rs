//! Cooperative build admission is a separate process mechanism from generation
//! admission; this binary exercises its public snapshot boundary and safety cap.
#![allow(missing_docs, clippy::expect_used, clippy::unwrap_used)]

use orbit_common::process::build_budget::read_waits;

#[test]
fn wait_snapshots_count_commands_but_credit_only_the_interval_union() {
    let directory = tempfile::tempdir().unwrap();
    for (name, start, elapsed) in [("a", 1000, 3000), ("b", 2000, 4000), ("c", 9000, 1000)] {
        std::fs::write(
            directory.path().join(format!("{name}.json")),
            serde_json::json!({
                "started_monotonic_ms": start, "elapsed_ms": elapsed,
            })
            .to_string(),
        )
        .unwrap();
    }
    std::fs::write(directory.path().join("invalid.json"), "partial").unwrap();
    std::fs::write(directory.path().join("unfinished.pending"), "partial").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        directory.path().join("a.json"),
        directory.path().join("link.json"),
    )
    .unwrap();
    let waits = read_waits(directory.path(), 4000);
    assert_eq!(waits.count, 3);
    assert_eq!(waits.total_ms, 8000);
    assert_eq!(waits.longest_ms, 4000);
    assert_eq!(
        waits.queued_wall_ms, 6000,
        "overlap cannot multiply runtime credit"
    );
    assert_eq!(
        waits.deadline_extension_ms, 4000,
        "credit never exceeds the original timeout"
    );
    assert_eq!(
        read_waits(directory.path(), 4000),
        waits,
        "a stopped heartbeat earns no further runtime credit"
    );
}
