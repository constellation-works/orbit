use std::io::Cursor;
use std::path::PathBuf;

use orbit_common::{NotFoundKind, OrbitError};
use serde_json::{Value, json};

use super::super::protocol::{
    BrokerRequest, CALL_FAILED, EntryPoint, FrameError, INVALID_INPUT, INVALID_REQUEST,
    MAX_REQUEST_BYTES, REFUSED, call_error_response, error_response, output_response,
    parse_request, read_frame, write_frame,
};

#[test]
fn a_written_frame_reads_back_whole() {
    let mut wire = Vec::new();
    write_frame(&mut wire, br#"{"schema_version":1}"#).expect("write frame");

    let body = read_frame(&mut Cursor::new(wire), MAX_REQUEST_BYTES).expect("read frame");

    assert_eq!(body, br#"{"schema_version":1}"#);
}

#[test]
fn an_oversized_frame_is_refused_before_its_body_is_read() {
    let declared = MAX_REQUEST_BYTES + 1;
    let mut wire = declared.to_be_bytes().to_vec();
    wire.extend_from_slice(b"body bytes the broker must not consume");
    let mut reader = Cursor::new(wire);

    let error = read_frame(&mut reader, MAX_REQUEST_BYTES).expect_err("frame over the cap");

    assert!(
        matches!(error, FrameError::TooLarge { declared: seen } if seen == declared),
        "unexpected frame error: {error:?}"
    );
    assert_eq!(
        reader.position(),
        4,
        "only the length prefix may be consumed from an oversized request"
    );
}

#[test]
fn a_short_frame_is_an_io_error() {
    let mut wire = 10u32.to_be_bytes().to_vec();
    wire.extend_from_slice(b"short");

    let error = read_frame(&mut Cursor::new(wire), MAX_REQUEST_BYTES).expect_err("short body");

    assert!(matches!(error, FrameError::Io(_)), "{error:?}");
}

#[test]
fn a_minimal_request_takes_the_protocol_defaults() {
    let body = json!({
        "schema_version": 1,
        "tool": "pulsar.post",
        "input": {"text": "hi"},
        "cwd": "/work/tree",
    })
    .to_string();

    assert_eq!(
        parse_request(body.as_bytes()),
        Ok(BrokerRequest {
            tool: "pulsar.post".to_string(),
            input: json!({"text": "hi"}),
            cwd: PathBuf::from("/work/tree"),
            workspace: None,
            entry_point: EntryPoint::Cli,
            dry_run: false,
        })
    );
}

#[test]
fn a_full_request_carries_every_caller_choice() {
    let body = json!({
        "schema_version": 1,
        "tool": "pulsar.post",
        "input": {},
        "cwd": "/work/tree/sub",
        "workspace": "orbit",
        "entry_point": "mcp",
        "dry_run": true,
    })
    .to_string();

    let request = parse_request(body.as_bytes()).expect("request parses");

    assert_eq!(request.workspace.as_deref(), Some("orbit"));
    assert_eq!(request.entry_point, EntryPoint::Mcp);
    assert!(request.dry_run);
}

#[test]
fn malformed_requests_are_rejected() {
    let valid = json!({"schema_version": 1, "tool": "pulsar.post", "input": {}, "cwd": "/w"});
    let with = |key: &str, value: Value| {
        let mut request = valid.clone();
        request[key] = value;
        request.to_string().into_bytes()
    };
    let without = |key: &str| {
        let mut request = valid.clone();
        request.as_object_mut().expect("object").remove(key);
        request.to_string().into_bytes()
    };
    for body in [
        b"not json".to_vec(),
        b"[1]".to_vec(),
        without("schema_version"),
        with("schema_version", json!(2)),
        without("tool"),
        with("tool", json!("")),
        without("input"),
        with("input", json!("text")),
        without("cwd"),
        with("cwd", json!("relative/dir")),
        with("workspace", json!(7)),
        with("entry_point", json!("http")),
        with("dry_run", json!("yes")),
    ] {
        assert!(
            parse_request(&body).is_err(),
            "accepted {}",
            String::from_utf8_lossy(&body)
        );
    }
}

#[test]
fn an_output_response_carries_only_the_output() {
    let response: Value = serde_json::from_slice(&output_response(json!({"posted": true})))
        .expect("response is JSON");

    assert_eq!(
        response,
        json!({"schema_version": 1, "ok": true, "output": {"posted": true}})
    );
}

#[test]
fn a_backend_error_keeps_its_own_code_retryability_and_detail() {
    let error = OrbitError::RemoteTool {
        code: "rate_limited".to_string(),
        message: "plugin tool 'pulsar.post' failed: slow down".to_string(),
        payload: json!({
            "code": "rate_limited",
            "message": "slow down",
            "retryable": true,
            "detail": {"after_ms": 500},
        }),
    };

    let response: Value =
        serde_json::from_slice(&call_error_response(&error)).expect("response is JSON");

    assert_eq!(response["ok"], false);
    assert_eq!(
        response["error"],
        json!({
            "code": "rate_limited",
            "message": "slow down",
            "retryable": true,
            "detail": {"after_ms": 500},
        })
    );
}

#[test]
fn host_errors_map_onto_the_broker_codes() {
    for (error, code) in [
        (OrbitError::PolicyDenied("not allowed".to_string()), REFUSED),
        (OrbitError::CapabilityDenied("agent".to_string()), REFUSED),
        (
            OrbitError::not_found(NotFoundKind::Tool, "pulsar.nope".to_string()),
            REFUSED,
        ),
        (
            OrbitError::InvalidInput("missing text".to_string()),
            INVALID_INPUT,
        ),
        (
            OrbitError::Execution("backend exited 1".to_string()),
            CALL_FAILED,
        ),
    ] {
        let response: Value =
            serde_json::from_slice(&call_error_response(&error)).expect("response is JSON");

        assert_eq!(response["error"]["code"], code, "{error}");
        assert_eq!(response["error"]["retryable"], false, "{error}");
        assert_eq!(response["error"]["detail"], Value::Null, "{error}");
    }
}

#[test]
fn an_error_response_is_a_structured_version_one_refusal() {
    let response: Value =
        serde_json::from_slice(&error_response(INVALID_REQUEST, "bad request", false))
            .expect("response is JSON");

    assert_eq!(response["schema_version"], 1);
    assert_eq!(response["ok"], false);
    assert_eq!(response["error"]["code"], INVALID_REQUEST);
    assert_eq!(response["error"]["retryable"], false);
}
