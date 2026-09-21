use serde_json::{Value, json};

use crate::OrbitRuntime;

use super::super::search_tools::search;

#[test]
fn search_tool_rejects_legacy_related_param() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let error = search(&runtime, json!({ "related": "ORB-00001" }))
        .expect_err("legacy related parameter should be rejected");

    assert!(error.to_string().contains("unknown parameter `related`"));
}

#[test]
fn search_tool_rejects_removed_document_path_param() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let error = search(&runtime, json!({ "path": "docs/guide.md" }))
        .expect_err("removed document path search must be rejected");

    assert!(error.to_string().contains("unknown parameter `path`"));
}

#[test]
fn search_tool_rejects_boolean_semantic_param() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let mut input = serde_json::Map::new();
    input.insert("query".to_string(), json!("anything"));
    input.insert("semantic".to_string(), json!(true));
    let error = search(&runtime, Value::Object(input))
        .expect_err("removed semantic parameter must be rejected");

    assert!(error.to_string().contains("unknown parameter `semantic`"));
}

#[test]
fn search_tool_rejects_retired_field_and_embedding_model_params() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let field_error = search(&runtime, json!({ "query": "anything", "field": "title" }))
        .expect_err("field parameter should be retired");
    assert!(
        field_error
            .to_string()
            .contains("unknown parameter `field`")
    );

    let model_error = search(
        &runtime,
        json!({ "query": "anything", "embedding_model": "bge-small" }),
    )
    .expect_err("embedding_model parameter should be retired");
    assert!(
        model_error
            .to_string()
            .contains("unknown parameter `embedding_model`")
    );
}

#[test]
fn search_tool_splits_comma_delimited_status_tokens() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let error = search(
        &runtime,
        json!({ "query": "anything", "status": "task:not-a-status,friction:resolved" }),
    )
    .expect_err("invalid task status should be parsed out of CSV");

    assert!(error.to_string().contains("`not-a-status`"));
    assert!(error.to_string().contains("`task`"));
}

#[test]
fn search_tool_rejects_removed_modes_even_with_valid_query() {
    let runtime = OrbitRuntime::in_memory().expect("runtime");
    for (key, value) in [("semantic", json!("task-id")), ("hybrid", json!(true))] {
        let mut input = json!({"query": "needle"});
        input[key] = value;
        let error = search(&runtime, input).expect_err("removed mode");
        assert!(
            error
                .to_string()
                .contains(&format!("unknown parameter `{key}`"))
        );
    }
}
