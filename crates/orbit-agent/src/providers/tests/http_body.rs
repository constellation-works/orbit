use super::super::http_body::{MAX_RESPONSE_BODY_BYTES, read_response_body};
use super::http_fixture::{ENDLESS_STREAM_CEILING, FixtureResponse, serve};
use crate::loop_engine::transport::TransportError;
use reqwest::blocking::{Client, Response};

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
fn chunked_body_without_length_stops_reading_at_the_limit() {
    let (response, server) = fetch(FixtureResponse::endless(200));

    assert_oversized(read_response_body(response));
    let served = server.join().expect("server thread");

    assert!(
        served[0].body_bytes_written < ENDLESS_STREAM_CEILING,
        "client must disconnect instead of draining the stream"
    );
}
