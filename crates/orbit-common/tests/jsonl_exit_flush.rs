//! The JSONL feed keeps the last records a process logs before it exits.
//!
//! The feed is written by a background worker that opens the file on its first
//! record, so a warning logged right before exit is still queued when the
//! process ends. The child is a fresh process that installs the global
//! subscriber, logs one warning, and exits through `std::process::exit`.
#![allow(missing_docs, clippy::expect_used, clippy::unwrap_used)]

use std::process::Command;

use orbit_common::observability::logging;
use orbit_common::test_env::{assert_child_test_exists, clear_inherited_authority};
use serde_json::Value;

const CHILD: &str = "warn_then_exit_child";
const EXIT_CODE: i32 = 7;

#[test]
fn final_record_before_process_exit_reaches_the_jsonl_feed() {
    assert_child_test_exists(CHILD);
    let home = tempfile::tempdir().expect("temp home");
    let mut command = Command::new(std::env::current_exe().unwrap());
    clear_inherited_authority(|key| {
        command.env_remove(key);
    });
    let output = command
        .args(["--exact", CHILD, "--ignored", "--nocapture"])
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env("RUST_LOG", "warn")
        .output()
        .expect("run child");
    assert_eq!(
        output.status.code(),
        Some(EXIT_CODE),
        "child must leave through process exit, not a test result:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let feed = home.path().join(".orbit/state/logs/orbit.jsonl");
    let raw = std::fs::read_to_string(&feed).unwrap_or_default();
    let records: Vec<Value> = raw
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|record| record["target"] == "orbit.test.exit")
        .collect();
    assert_eq!(
        records.len(),
        1,
        "the warning logged right before exit must reach {feed:?}: {raw:?}"
    );
    assert_eq!(records[0]["level"], "WARN");
    assert_eq!(records[0]["fields"]["code"], EXIT_CODE);
}

#[test]
#[ignore = "re-executed by final_record_before_process_exit_reaches_the_jsonl_feed"]
fn warn_then_exit_child() {
    logging::init_default_subscriber("warn");
    tracing::warn!(target: "orbit.test.exit", code = EXIT_CODE, "last words before exit");
    logging::exit(EXIT_CODE);
}
