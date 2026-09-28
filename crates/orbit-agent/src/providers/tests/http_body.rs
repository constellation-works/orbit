use reqwest::blocking::{Client, Response};

use super::super::http_body::{
    MAX_ERROR_BODY_BYTES, MAX_RESPONSE_BODY_BYTES, body_diagnostic, read_error_body,
    read_response_body,
};
use super::http_fixture::{ENDLESS_STREAM_CEILING, FixtureBody, FixtureResponse, serve};
use crate::loop_engine::transport::TransportError;

fn fetch(
    response: FixtureResponse,
) -> (
    Response,
    std::thread::JoinHandle<Vec<super::http_fixture::ServedRequest>>,
) {
    let (base_url, server) = serve(vec![response]);
    let response = Client::new().get(base_url).send().expect("send request");
    (response, server)
}

fn assert_oversized(result: Result<Vec<u8>, TransportError>) {
    match result {
        Err(TransportError::Decode(message)) => assert!(
            message.contains(&MAX_RESPONSE_BODY_BYTES.to_string()),
            "limit error should name the cap: {message}"
        ),
        Err(other) => panic!("expected oversized-body decode error, got {other}"),
        Ok(bytes) => panic!("oversized body was accepted ({} bytes)", bytes.len()),
    }
}

#[test]
fn body_at_the_limit_is_returned_whole() {
    let body = vec![b'a'; MAX_RESPONSE_BODY_BYTES];
    let (response, server) = fetch(FixtureResponse {
        status: 200,
        body: FixtureBody::Sized(body.clone()),
    });

    let bytes = read_response_body(response).expect("body within limit");
    server.join().expect("server thread");

    assert_eq!(bytes, body);
}

#[test]
fn declared_oversized_length_is_refused_before_reading() {
    // The fixture sends no body: reading would fail with a network error, so
    // a limit error proves the Content-Length check refused it up front.
    let (response, server) = fetch(FixtureResponse {
        status: 200,
        body: FixtureBody::DeclaredOnly(MAX_RESPONSE_BODY_BYTES as u64 + 1),
    });

    assert_oversized(read_response_body(response));
    server.join().expect("server thread");
}

#[test]
fn chunked_body_without_length_stops_reading_at_the_limit() {
    let (response, server) = fetch(FixtureResponse::endless(200));

    assert_oversized(read_response_body(response));
    let served = server.join().expect("server thread");

    assert!(
        served[0].body_bytes_written < ENDLESS_STREAM_CEILING,
        "client must disconnect instead of draining the stream"
    );
}

#[test]
fn error_body_keeps_a_bounded_prefix_of_an_endless_stream() {
    let (response, server) = fetch(FixtureResponse::endless(500));

    let body = read_error_body(response).expect("bounded error body");
    let served = server.join().expect("server thread");

    let (prefix, marker) = body.split_at(MAX_ERROR_BODY_BYTES);
    assert!(prefix.bytes().all(|b| b == b'x'));
    assert!(marker.contains("truncated"), "marker missing: {marker}");
    assert!(served[0].body_bytes_written < ENDLESS_STREAM_CEILING);
}

#[test]
fn short_error_body_is_returned_verbatim() {
    let (response, server) = fetch(FixtureResponse::json(429, r#"{"error":"slow down"}"#));

    let body = read_error_body(response).expect("error body");
    server.join().expect("server thread");

    assert_eq!(body, r#"{"error":"slow down"}"#);
}

#[test]
fn body_diagnostic_truncates_long_buffers() {
    assert_eq!(body_diagnostic(b"short"), "short");

    let long = vec![b'y'; MAX_ERROR_BODY_BYTES * 2];
    let rendered = body_diagnostic(&long);
    assert!(rendered.len() < MAX_ERROR_BODY_BYTES + 64);
    assert!(rendered.starts_with(&"y".repeat(MAX_ERROR_BODY_BYTES)));
    assert!(rendered.contains("truncated"));
}
