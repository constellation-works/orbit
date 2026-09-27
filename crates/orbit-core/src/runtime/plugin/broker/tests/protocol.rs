use std::io::Cursor;

use serde_json::{Value, json};

use super::super::protocol::{
    FrameError, INVALID_REQUEST, MAX_REQUEST_BYTES, error_response, parse_request, read_frame,
    write_frame,
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
fn a_version_one_request_names_its_tool() {
    let body = json!({"schema_version": 1, "tool": "pulsar.post", "input": {}}).to_string();

    assert_eq!(
        parse_request(body.as_bytes()),
        Ok("pulsar.post".to_string())
    );
}

#[test]
fn malformed_requests_are_rejected() {
    for body in [
        b"not json".to_vec(),
        b"[1]".to_vec(),
        json!({"tool": "pulsar.post"}).to_string().into_bytes(),
        json!({"schema_version": 2, "tool": "pulsar.post"})
            .to_string()
            .into_bytes(),
        json!({"schema_version": 1}).to_string().into_bytes(),
        json!({"schema_version": 1, "tool": ""})
            .to_string()
            .into_bytes(),
    ] {
        assert!(
            parse_request(&body).is_err(),
            "accepted {}",
            String::from_utf8_lossy(&body)
        );
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
