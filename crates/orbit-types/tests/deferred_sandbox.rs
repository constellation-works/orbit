//! Deferred-sandbox records decoded through the public workflow API.

// Test assertions unwrap results, following the test-only crate-root exemption.
#![allow(clippy::expect_used)]

use orbit_types::workflow::{
    BUBBLEWRAP_DEFERRAL_PREFIX, BUBBLEWRAP_NAMESPACE_PROBE, BUBBLEWRAP_NAMESPACE_REFUSAL,
    deferred_sandbox_validation,
};
use serde_json::{Value, json};

const GATE: &str = "make ci-test-affected";
const BASE: &str = "e867385d9feda42090fdeb6fd83b8fa0c4d4eb4d";

/// A passing gate whose only gap is a Bubblewrap deferral backed by a failing
/// namespace probe: the sanctioned case, with every field present.
fn sanctioned_record() -> Value {
    json!({
        "command": GATE,
        "exit_code": 0,
        "tests_run": 42,
        "base": BASE,
        "notices": [format!("{BUBBLEWRAP_DEFERRAL_PREFIX} tests/sandboxed.rs skipped")],
        "probe": {
            "command": BUBBLEWRAP_NAMESPACE_PROBE,
            "exit_code": 1,
            "output": BUBBLEWRAP_NAMESPACE_REFUSAL,
        },
    })
}

/// The refusal reason for `record` as the implement step boundary reports it.
fn refusal(record: Value) -> String {
    let output = json!({ "deferred_sandbox_validation": record });
    deferred_sandbox_validation(&output, &[GATE.to_string()], Some(BASE))
        .expect_err("a record missing a required field is refused")
}

#[test]
fn a_complete_sanctioned_record_is_accepted() {
    let output = json!({ "deferred_sandbox_validation": sanctioned_record() });

    let accepted = deferred_sandbox_validation(&output, &[GATE.to_string()], Some(BASE))
        .expect("every field present, so the record decodes and passes its checks");

    assert!(accepted.is_some(), "the sanctioned record is kept");
}

#[test]
fn a_record_missing_probe_command_names_the_probe() {
    let mut record = sanctioned_record();
    record["probe"]
        .as_object_mut()
        .expect("probe is an object")
        .remove("command");

    let reason = refusal(record);

    assert!(
        reason.contains("record: probe: missing field `command`"),
        "{reason}"
    );
}

#[test]
fn a_record_missing_the_top_level_command_names_the_command() {
    let mut record = sanctioned_record();
    record
        .as_object_mut()
        .expect("record is an object")
        .remove("command");

    let reason = refusal(record);

    assert!(
        reason.contains("record: missing field `command`"),
        "{reason}"
    );
    assert!(!reason.contains("record: probe"), "{reason}");
}
