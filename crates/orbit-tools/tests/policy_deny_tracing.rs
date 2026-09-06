#![allow(missing_docs)]
// Tests use unwrap/expect to keep fixture setup readable.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::time::{Duration, Instant};

use orbit_common::OrbitError;
use orbit_common::observability::logging::init_default_subscriber;
use orbit_common::test_env::{INHERITED_AUTHORITY_ENV, unset};
use orbit_tools::{ToolContext, ToolRegistry};
use serde_json::{Value, json};
use tempfile::tempdir;

#[test]
fn policy_denials_emit_redacted_jsonl_tracing_events() {
    let _env_guard = unset(INHERITED_AUTHORITY_ENV.iter().copied());
    let home = tempdir().expect("create temp home");
    let log_path = home.path().join(".orbit/state/logs/orbit.jsonl");

    // SAFETY: this single-test integration binary mutates the process
    // environment before installing Orbit's subscriber or starting its writer.
    unsafe {
        std::env::set_var("HOME", home.path());
        std::env::set_var("RUST_LOG", "warn");
    }
    init_default_subscriber("warn");

    let mut registry = ToolRegistry::new();
    registry.register_builtins();

    let proc_ctx = ToolContext {
        proc_allowed_programs: vec!["echo".to_string()],
        ..Default::default()
    };
    let err = registry
        .execute("proc.spawn", &proc_ctx, json!({ "program": "sh" }))
        .expect_err("proc spawn should be denied");
    assert!(matches!(err, OrbitError::PolicyDenied(_)));

    let events = wait_for_policy_events(&log_path, 1);
    let proc_event = events
        .iter()
        .find(|event| event["fields"]["tool"] == "proc.spawn")
        .expect("proc deny tracing event");
    assert_eq!(proc_event["fields"]["path"], "sh");
    assert_eq!(proc_event["fields"]["profile"], "proc.allowed_programs");
    assert_eq!(proc_event["fields"]["matched_rule"], "echo");
}

fn wait_for_policy_events(log_path: &std::path::Path, expected: usize) -> Vec<Value> {
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        let events = fs::read_to_string(log_path)
            .ok()
            .map(|raw| {
                raw.lines()
                    .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                    .filter(|event| event["target"] == "orbit.policy.deny")
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if events.len() >= expected {
            return events;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("timed out waiting for {expected} policy deny events at {log_path:?}");
}
