#![allow(missing_docs)]

use orbit_types::tool::ExecutionResult;
use serde_json::{Value, json};

use super::super::response::AgentResponseStatus;
use super::super::response::envelope::*;

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

fn success_envelope() -> Value {
    json!({
        "schemaVersion": 1,
        "status": "success",
        "result": {"ok": true},
        "error": null
    })
}

fn failed_envelope() -> Value {
    json!({
        "schemaVersion": 1,
        "status": "failed",
        "result": {},
        "error": {"code": "E", "message": "failed", "details": null}
    })
}

fn timeout_envelope() -> Value {
    json!({
        "schemaVersion": 1,
        "status": "timeout",
        "result": {},
        "error": {"code": "deadline", "message": "timed out", "details": null}
    })
}

fn production_budget() -> EnvelopeDiscoveryBudget {
    EnvelopeDiscoveryBudget::production()
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
fn banner_wrapped_json_finds_the_envelope_without_rescanning_nested_braces() {
    let nested_result = json!({
        "schemaVersion": 1,
        "status": "success",
        "result": {"inner": {"a": {"b": {"c": 1}}}},
        "error": null
    });
    let stdout = format!("tool banner: still running\n{nested_result}");

    response_envelope_protocol_check(&stdout).expect("banner-wrapped envelope is a frame");

    let (found, stats) = discover_agent_response_envelope_with_stats(&stdout, production_budget())
        .expect("discover");
    let envelope = found.expect("envelope after banner");
    assert_eq!(envelope.status, "success");
    assert!(
        stats.parse_attempts <= 3,
        "whole-string miss plus one object parse, not one parse per nested brace: {stats:?}"
    );
    assert!(
        stats.nodes_visited < ENVELOPE_DISCOVERY_MAX_NODES,
        "{stats:?}"
    );
}

#[test]
fn truncated_large_payload_fails_closed_instead_of_succeeding() {
    let mut stdout = String::from('{');
    for index in 0..5_000 {
        stdout.push_str(&format!("\"k{index}\":{{"));
    }

    let error = response_envelope_protocol_check(&stdout)
        .expect_err("truncated payload is not a completed envelope");
    assert_limit_error(&error);
    assert!(
        parse_and_validate_response(&exec(&stdout, Some(0))).is_err(),
        "must not parse a truncated payload as success"
    );
    assert_eq!(peek_response_status(&stdout), None);
    assert_eq!(peek_declared_response_failure(&stdout), None);
}

#[test]
fn brace_dense_text_hits_the_documented_parse_bound() {
    let raw = "{".repeat(10_000);
    let wrapped = json!({ "result": raw }).to_string();

    let error = discover_agent_response_envelope_with_stats(&raw, production_budget())
        .expect_err("brace-dense text must exhaust the parse bound");
    assert_limit_error(&error);
    assert!(error.to_string().contains("parse attempts"));

    let error = discover_declared_response_failure_with_stats(&wrapped, production_budget())
        .expect_err("declared-failure discovery shares the parse bound");
    assert_limit_error(&error);
    assert_eq!(peek_declared_response_failure(&wrapped), None);
    let error = response_envelope_protocol_check(&wrapped)
        .expect_err("string-field brace density is a limit failure, not a missing envelope");
    assert_limit_error(&error);
}

#[test]
fn deeply_nested_preferred_fields_still_find_the_envelope_and_declared_failure() {
    let mut envelope = success_envelope();
    let mut failure = failed_envelope();
    for key in [
        "output",
        "final_message",
        "final",
        "content",
        "messages",
        "message",
        "response",
        "result",
        "structured_output",
        "output",
        "result",
        "structured_output",
    ] {
        envelope = json!({ key: envelope });
        failure = json!({ key: failure });
    }

    let stdout = envelope.to_string();
    let (parsed, status, _) =
        parse_and_validate_response(&exec(&stdout, Some(0))).expect("nested preferred keys");
    assert_eq!(status, AgentResponseStatus::Success);
    assert_eq!(parsed.result.expect("result")["ok"], true);

    let failure_stdout = failure.to_string();
    let declared =
        peek_declared_response_failure(&failure_stdout).expect("nested declared failure");
    assert_eq!(declared.status, "failed");
    assert_eq!(declared.error.expect("error").code, "E");
}

#[test]
fn large_valid_envelope_is_found_when_its_opening_brace_is_not_among_the_last_few() {
    let mut nested = json!({"leaf": true});
    for index in 0..80 {
        nested = json!({ format!("n{index}"): nested });
    }
    let envelope = json!({
        "schemaVersion": 1,
        "status": "success",
        "result": nested,
        "error": null
    });
    let mut tail = Vec::new();
    for _ in 0..2_000 {
        tail.push(json!({}));
    }
    let wrapper = json!({
        "payload": envelope,
        "z_tail": tail
    });
    let stdout = format!("note: decoy objects follow the envelope\n{wrapper}");

    let (found, stats) = discover_agent_response_envelope_with_stats(&stdout, production_budget())
        .expect("discover");
    assert_eq!(found.expect("early envelope").status, "success");
    assert!(
        stats.parse_attempts <= 3,
        "suffix scan must consume the wrapper once, not the last few nested braces: {stats:?}"
    );
    response_envelope_protocol_check(&stdout).expect("frame still holds");
}

#[test]
fn preferred_named_children_are_not_walked_twice_for_envelope_or_declared_failure() {
    let width = 100;
    let envelope_payload = json!({
        "structured_output": dummy_object(width),
        "zz_other": success_envelope()
    });
    let failure_payload = json!({
        "structured_output": dummy_object(width),
        "zz_other": failed_envelope()
    });
    // root + preferred object + `width` leaves + sibling envelope.
    let expected_nodes = 1 + 1 + width as u32 + 1;

    let (found, stats) = discover_agent_response_envelope_with_stats(
        &envelope_payload.to_string(),
        production_budget(),
    )
    .expect("envelope search");
    assert_eq!(found.expect("sibling envelope").status, "success");
    assert_eq!(
        stats.nodes_visited,
        expected_nodes,
        "re-walking structured_output would add another {} nodes: {stats:?}",
        1 + width
    );

    let (found, stats) = discover_declared_response_failure_with_stats(
        &failure_payload.to_string(),
        production_budget(),
    )
    .expect("declared-failure search");
    assert_eq!(found.expect("sibling failure").status, "failed");
    assert_eq!(stats.nodes_visited, expected_nodes);

    let tight = EnvelopeDiscoveryBudget::new(ENVELOPE_DISCOVERY_MAX_PARSE_ATTEMPTS, expected_nodes);
    discover_agent_response_envelope_with_stats(&envelope_payload.to_string(), tight)
        .expect("one-pass budget is enough")
        .0
        .expect("found");

    let too_tight = EnvelopeDiscoveryBudget::new(
        ENVELOPE_DISCOVERY_MAX_PARSE_ATTEMPTS,
        expected_nodes.saturating_sub(width as u32),
    );
    let error =
        discover_agent_response_envelope_with_stats(&envelope_payload.to_string(), too_tight)
            .expect_err("a one-pass-minus-the-sibling budget must not invent a hit");
    assert_limit_error(&error);
    let (_, status, _) = parse_and_validate_response(&exec(&envelope_payload.to_string(), Some(0)))
        .expect("production bound still finds it");
    assert_eq!(status, AgentResponseStatus::Success);
}

#[test]
fn structured_output_still_outranks_result_for_envelopes_and_declared_failures() {
    let stdout = json!({
        "result": failed_envelope(),
        "structured_output": success_envelope()
    })
    .to_string();
    let (envelope, status, _) =
        parse_and_validate_response(&exec(&stdout, Some(0))).expect("preferred key wins");
    assert_eq!(status, AgentResponseStatus::Success);
    assert_eq!(envelope.result.expect("result")["ok"], true);

    let stdout = json!({
        "result": failed_envelope(),
        "structured_output": timeout_envelope()
    })
    .to_string();
    let declared = peek_declared_response_failure(&stdout).expect("preferred failure");
    assert_eq!(declared.status, "timeout");
    assert_eq!(declared.error.expect("error").code, "deadline");
}

#[test]
fn array_documents_keep_last_to_first_envelope_precedence() {
    let stdout = json!([failed_envelope(), success_envelope()]).to_string();
    assert_eq!(peek_response_status(&stdout).as_deref(), Some("success"));

    let stdout = json!([success_envelope(), failed_envelope()]).to_string();
    assert_eq!(peek_response_status(&stdout).as_deref(), Some("failed"));
    let declared = peek_declared_response_failure(&stdout).expect("last failed item");
    assert_eq!(declared.status, "failed");
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

#[test]
fn production_node_bound_fails_closed_on_a_huge_decoy_array() {
    let mut stdout = String::from('[');
    for index in 0..70_000 {
        if index > 0 {
            stdout.push(',');
        }
        stdout.push('0');
    }
    stdout.push(']');

    let error = response_envelope_protocol_check(&stdout)
        .expect_err("70k decoy nodes exceed the documented node bound");
    assert_limit_error(&error);
    assert!(error.to_string().contains("traversed nodes"));
    assert!(
        parse_and_validate_response(&exec(&stdout, Some(0))).is_err(),
        "must not classify an unexamined decoy array as success"
    );
}

#[test]
fn declared_failure_inside_a_string_field_still_parses() {
    let result = format!("could not continue\n{}", failed_envelope());
    let stdout = json!({ "result": result }).to_string();
    let declared = peek_declared_response_failure(&stdout).expect("string-embedded failure");
    assert_eq!(declared.status, "failed");
    assert_eq!(peek_response_status(&stdout).as_deref(), Some("failed"));
}
