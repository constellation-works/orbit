//! The friction write surface, exercised through the registered tools
//! [ORB-10590].
//!
//! These are the boundary tests for the record handle: what an author can set,
//! what the surface refuses, and what a caller who sets nothing gets.

use orbit_common::governance::friction::FRICTION_TITLE_MAX_CHARS;
use orbit_common::test_fixtures::TEST_CODEX_MODEL;
use serde_json::{Value, json};

use super::super::test_support::{invalid_input_message, run_tool_as_operator, test_runtime};

/// A structured report whose opening line labels a section rather than the
/// record — the shape derivation has to see through.
const SECTIONED_BODY: &str = "## What happened\n\nThe worker exited before claiming the run.\n\n\
                              ## Evidence\n\nOne log line.";

fn add(input: Value) -> Result<Value, orbit_common::OrbitError> {
    let (_temp, runtime, _repo) = test_runtime();
    run_tool_as_operator(&runtime, "orbit.friction.add", input)
}

#[test]
fn add_records_the_authors_title() {
    let record = add(json!({
        "body": SECTIONED_BODY,
        "title": "Worker exits before claiming its run",
        "model": TEST_CODEX_MODEL,
    }))
    .expect("add with title");

    assert_eq!(
        record["title"],
        json!("Worker exits before claiming its run")
    );
}

#[test]
fn add_without_a_title_falls_back_to_the_bodys_subject() {
    let record = add(json!({ "body": SECTIONED_BODY, "model": TEST_CODEX_MODEL }))
        .expect("add without title");

    assert_eq!(
        record["title"],
        json!("The worker exited before claiming the run.")
    );
}

#[test]
fn add_refuses_a_non_string_task_id() {
    let message = invalid_input_message(add(json!({
        "body": SECTIONED_BODY,
        "task_id": 123,
        "model": TEST_CODEX_MODEL,
    })));

    assert!(message.contains("`task_id`"), "{message}");
}

#[test]
fn add_refuses_a_title_too_long_to_read_in_a_list() {
    let message = invalid_input_message(add(json!({
        "body": SECTIONED_BODY,
        "title": "x".repeat(FRICTION_TITLE_MAX_CHARS + 1),
        "model": TEST_CODEX_MODEL,
    })));

    assert!(message.contains("`title`"), "{message}");
    assert!(
        message.contains(&FRICTION_TITLE_MAX_CHARS.to_string()),
        "{message}"
    );
}

#[test]
fn add_refuses_a_blank_title() {
    let message = invalid_input_message(add(json!({
        "body": SECTIONED_BODY,
        "title": "   ",
        "model": TEST_CODEX_MODEL,
    })));

    assert!(message.contains("must not be blank"), "{message}");
}

#[test]
fn a_multi_line_title_is_stored_as_one_line() {
    let record = add(json!({
        "body": SECTIONED_BODY,
        "title": "Worker exits\nbefore claiming its run",
        "model": TEST_CODEX_MODEL,
    }))
    .expect("add with multi-line title");

    assert_eq!(
        record["title"],
        json!("Worker exits before claiming its run")
    );
}

#[test]
fn update_retitles_a_record_without_touching_its_body() {
    let (_temp, runtime, _repo) = test_runtime();
    let seeded = run_tool_as_operator(
        &runtime,
        "orbit.friction.add",
        json!({ "body": SECTIONED_BODY, "model": TEST_CODEX_MODEL }),
    )
    .expect("seed record");
    let id = seeded["id"].as_str().expect("record id");

    let updated = run_tool_as_operator(
        &runtime,
        "orbit.friction.update",
        json!({ "id": id, "title": "Worker exits before claiming its run" }),
    )
    .expect("retitle");

    assert_eq!(
        updated["title"],
        json!("Worker exits before claiming its run")
    );
    assert_eq!(updated["body"], seeded["body"]);
}

#[test]
fn an_empty_update_title_restores_derivation() {
    let (_temp, runtime, _repo) = test_runtime();
    let seeded = run_tool_as_operator(
        &runtime,
        "orbit.friction.add",
        json!({ "body": SECTIONED_BODY, "title": "Set by hand", "model": TEST_CODEX_MODEL }),
    )
    .expect("seed record");
    let id = seeded["id"].as_str().expect("record id");

    let updated = run_tool_as_operator(
        &runtime,
        "orbit.friction.update",
        json!({ "id": id, "title": "" }),
    )
    .expect("clear title");

    assert_eq!(
        updated["title"],
        json!("The worker exited before claiming the run.")
    );
}

#[test]
fn list_notes_empty_multi_word_substring_miss() {
    let (_temp, runtime, _repo) = test_runtime();
    let seeded = run_tool_as_operator(
        &runtime,
        "orbit.friction.add",
        json!({
            "title": "EnvGuard dropped its restore",
            "body": "workspace_init lost the parallel env snapshot while EnvGuard was armed.",
            "tags": ["tooling"],
            "model": TEST_CODEX_MODEL,
        }),
    )
    .expect("seed open friction");
    let id = seeded["id"].as_str().expect("record id");

    let miss = run_tool_as_operator(
        &runtime,
        "orbit.friction.list",
        json!({ "status": "open", "q": "EnvGuard workspace_init" }),
    )
    .expect("multi-word list");

    assert_eq!(miss["records"], json!([]));
    let notes = miss["notes"].as_array().expect("notes array");
    assert_eq!(notes.len(), 1);
    let note = notes[0].as_str().expect("note text");
    assert!(note.contains("single case-insensitive substring"), "{note}");
    assert!(note.contains("not proof the corpus is empty"), "{note}");
    assert!(note.contains("EnvGuard"), "{note}");
    assert!(note.contains("workspace_init"), "{note}");

    let hit = run_tool_as_operator(
        &runtime,
        "orbit.friction.list",
        json!({ "status": "open", "q": "EnvGuard" }),
    )
    .expect("single-term list");

    let records = hit.as_array().expect("historical array shape");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["id"], json!(id));

    let unmatched = run_tool_as_operator(
        &runtime,
        "orbit.friction.list",
        json!({ "status": "open", "q": "EnvGuardUniqueMiss" }),
    )
    .expect("single-term miss");
    assert!(
        unmatched
            .as_array()
            .is_some_and(|records| records.is_empty()),
        "single-term empty pages keep the array shape: {unmatched}"
    );
}

#[test]
fn update_still_requires_at_least_one_mutable_field() {
    let (_temp, runtime, _repo) = test_runtime();
    let seeded = run_tool_as_operator(
        &runtime,
        "orbit.friction.add",
        json!({ "body": SECTIONED_BODY, "model": TEST_CODEX_MODEL }),
    )
    .expect("seed record");
    let id = seeded["id"].as_str().expect("record id");

    let message = invalid_input_message(run_tool_as_operator(
        &runtime,
        "orbit.friction.update",
        json!({ "id": id }),
    ));

    assert!(message.contains("`title`"), "{message}");
}
