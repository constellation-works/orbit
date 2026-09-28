use super::super::messages_transport::AnthropicMessagesTransport;
use crate::loop_engine::transport::{
    CacheHint, ContentBlock, LoopTransport, Message, TransportError, TurnRequest,
};
use crate::providers::http_body::{MAX_ERROR_BODY_BYTES, MAX_RESPONSE_BODY_BYTES};
use crate::providers::tests::http_fixture::{ENDLESS_STREAM_CEILING, FixtureResponse, serve};

fn transport(base_url: &str) -> AnthropicMessagesTransport {
    AnthropicMessagesTransport::new("test-key", "claude-test")
        .expect("transport")
        .with_endpoint(format!("{base_url}/v1/messages"))
}

fn send(
    transport: &AnthropicMessagesTransport,
) -> Result<crate::loop_engine::transport::TurnResponse, TransportError> {
    let messages = [Message::user_text("hello")];
    transport.send_turn(&TurnRequest {
        system: None,
        messages: &messages,
        tools: &[],
        cache_hint: CacheHint::None,
        max_response_tokens: 16,
    })
}

#[test]
fn under_limit_response_decodes() {
    let (base_url, server) = serve(vec![FixtureResponse::json(
        200,
        r#"{"content":[{"type":"text","text":"ok"}],"stop_reason":"end_turn","usage":{"input_tokens":3,"output_tokens":1}}"#,
    )]);

    let response = send(&transport(&base_url)).expect("send turn");
    server.join().expect("server thread");

    assert!(matches!(&response.content[..], [ContentBlock::Text { text }] if text == "ok"));
    assert_eq!(response.usage.input_tokens, 3);
}

#[test]
fn oversized_chunked_success_is_refused_without_draining() {
    let (base_url, server) = serve(vec![FixtureResponse::endless(200)]);

    let err = send(&transport(&base_url)).expect_err("oversized body");
    let served = server.join().expect("server thread");

    let TransportError::Decode(message) = err else {
        panic!("expected oversized-body decode error, got {err}");
    };
    assert!(message.contains(&MAX_RESPONSE_BODY_BYTES.to_string()));
    assert!(served[0].body_bytes_written < ENDLESS_STREAM_CEILING);
}

#[test]
fn oversized_error_status_keeps_a_bounded_body() {
    let (base_url, server) = serve(vec![FixtureResponse::endless(529)]);

    let err = send(&transport(&base_url)).expect_err("bad status");
    let served = server.join().expect("server thread");

    let TransportError::BadStatus { status, body } = err else {
        panic!("expected bad status, got {err}");
    };
    assert_eq!(status, 529);
    assert!(body.len() < MAX_ERROR_BODY_BYTES + 64);
    assert!(served[0].body_bytes_written < ENDLESS_STREAM_CEILING);
}
