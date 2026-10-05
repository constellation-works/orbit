#![allow(missing_docs)]

use super::super::response::envelope::*;
use orbit_types::tool::ExecutionResult;
use serde_json::{Value, json};

fn exec(stdout: &str, exit_code: Option<i32>) -> ExecutionResult {
    ExecutionResult {
        success: exit_code == Some(0),
        timed_out: false,
        stdout: stdout.to_string(),
        stderr: String::new(),
        exit_code,
        duration_ms: 12,
        output: None,
    }
}

fn assert_limit_error(error: &orbit_common::OrbitError) {
    let message = error.to_string();
    assert!(
        message.contains("response envelope discovery exceeded a work limit"),
        "expected an explicit discovery-limit failure, got {message}"
    );
}

fn dummy_object(width: usize) -> Value {
    let mut map = serde_json::Map::new();
    for index in 0..width {
        map.insert(format!("k{index}"), json!(index));
    }
    Value::Object(map)
}

#[test]
fn exhausted_limits_do_not_synthesize_success() {
    let stdout = json!({
        "is_error": true,
        "subtype": "error_max_turns",
        "terminal_reason": "max_turns",
        "structured_output": dummy_object(70_000)
    })
    .to_string();
    let error = parse_and_validate_response(&exec(&stdout, Some(0)))
        .expect_err("limit exhaustion is a parse failure");
    assert_limit_error(&error);
    assert!(
        synthesize_response(&exec(&stdout, Some(0))).is_none(),
        "a limit error must not be rewritten into a synthesized terminal failure"
    );
}
