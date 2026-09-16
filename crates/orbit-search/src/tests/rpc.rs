//! Unit tests for the RPC envelope's boundary translator (ORB-10013) and
//! response correlation (ORB-11705) — sibling layout.

use orbit_common::OrbitError;

use crate::rpc::{
    RpcError, RpcRequest, RpcResponse, RpcResult, UNCORRELATED_REQUEST_ID, rpc_error_to_orbit,
    unparsed_request_id,
};

#[test]
fn token_count_request_and_response_are_batched() {
    let request = RpcRequest::TokenCount {
        id: 7,
        texts: vec!["one".to_string(), "two words".to_string()],
    };
    assert_eq!(
        serde_json::to_value(&request).unwrap(),
        serde_json::json!({
            "method": "token_count",
            "id": 7,
            "texts": ["one", "two words"]
        })
    );

    let response = RpcResponse::Result {
        id: 7,
        result: RpcResult::TokenCount { tokens: vec![1, 2] },
    };
    assert_eq!(
        serde_json::to_value(&response).unwrap(),
        serde_json::json!({"id": 7, "result": {"tokens": [1, 2]}})
    );
}

#[test]
fn rpc_error_to_orbit_renders_code_and_message_into_execution() {
    let error = RpcError {
        code: "embed_failed".to_string(),
        message: "model not loaded".to_string(),
    };
    assert!(matches!(
        rpc_error_to_orbit(error),
        OrbitError::Execution(m) if m == "search companion embed_failed: model not loaded"
    ));
}

#[test]
fn unparsed_request_id_correlates_only_a_trustworthy_object_id() {
    // A malformed line may still identify the caller it belongs to. Accept
    // that only when the line is a JSON object carrying an unsigned integer
    // `id`; everything else must stay uncorrelated so a client never reads a
    // garbled line as an answer to one of its in-flight requests.
    let table = [
        (r#"{"id":7,"method":"nope"}"#, 7),
        (r#"{"method":"embed","id":42}"#, 42),
        (r#"  {"id":1}  "#, 1),
        (r#"{"id":0}"#, UNCORRELATED_REQUEST_ID),
        (r#"{"method":"embed"}"#, UNCORRELATED_REQUEST_ID),
        (r#"{"id":-3}"#, UNCORRELATED_REQUEST_ID),
        (r#"{"id":1.5}"#, UNCORRELATED_REQUEST_ID),
        (r#"{"id":"7"}"#, UNCORRELATED_REQUEST_ID),
        (r#"[{"id":7}]"#, UNCORRELATED_REQUEST_ID),
        (r#"{"id":7"#, UNCORRELATED_REQUEST_ID),
        ("this is not json", UNCORRELATED_REQUEST_ID),
        ("", UNCORRELATED_REQUEST_ID),
    ];
    for (line, expected) in table {
        assert_eq!(
            unparsed_request_id(line),
            expected,
            "line {line:?} correlated to the wrong id"
        );
    }
}

#[test]
fn rpc_response_reports_the_id_it_answers() {
    let result = RpcResponse::Result {
        id: 9,
        result: RpcResult::Exit { ok: true },
    };
    let error = RpcResponse::Error {
        id: 11,
        error: RpcError {
            code: "invalid_request".to_string(),
            message: "bad".to_string(),
        },
    };
    assert_eq!(result.id(), 9);
    assert_eq!(error.id(), 11);
}
