//! Timing the public incident grouping boundary on the capped scoreboard shape.
#![allow(clippy::expect_used, clippy::unwrap_used, missing_docs)]

use chrono::DateTime;
use orbit_store::contracts::group_failure_incidents;
use orbit_types::telemetry::AuditEvent;
use std::time::{Duration, Instant};

#[test]
#[ignore = "explicit release-mode timing benchmark; run with --release --ignored"]
fn capped_scoreboard_population_groups_within_half_a_second() {
    let _subscriber = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_test_writer()
            .with_ansi(false)
            .without_time()
            .finish(),
    );
    let failures: Vec<AuditEvent> = (0..10_000).map(|id| {
        // Children follow their leaves, while 8,800 independent runs have no
        // edge. About 332k whitespace tokens reproduce the expensive scan.
        let citation = if (8_800..10_000).contains(&id) {
            format!("quoted leaf (`jrun-fixture-{}`),", id - 8_800)
        } else { String::new() };
        serde_json::from_value(serde_json::json!({
            "id":id, "execution_id":format!("execution-{id}"),
            "timestamp":DateTime::from_timestamp(1_780_000_000 + i64::from(id >= 8_800), 0).unwrap(),
            "command":"tool", "subcommand":"run", "tool_name":"orbit.fixture.run",
            "target_type":null, "target_id":null, "role":"codex", "status":"failure",
            "exit_code":1, "duration_ms":1, "working_directory":".",
            "arguments_json":null, "stdout_truncated":null, "stderr_truncated":null,
            "error_message":format!("database connection failed {} {citation}", "evidence ".repeat(29)),
            "host":null, "pid":1, "session_id":null,
            "job_run_id":format!("jrun-fixture-{id}"),
        })).unwrap()
    }).collect();
    let started = Instant::now();
    let incidents = group_failure_incidents(&failures);
    let elapsed = started.elapsed();
    assert_eq!(incidents.len(), 8_800);
    assert_eq!(incidents.iter().map(|i| i.event_count).sum::<u64>(), 10_000);
    tracing::info!(
        events = 10_000,
        citations = 1_200,
        elapsed_ms = elapsed.as_secs_f64() * 1000.0,
        "capped population grouped"
    );
    assert!(
        elapsed < Duration::from_millis(500),
        "capped scoreboard grouping exceeded 500ms: {elapsed:?}"
    );
}
