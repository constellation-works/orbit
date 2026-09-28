use super::super::chat_completions_transport::{OpenAiCompatTransport, turn_usage_from_wire};
use super::super::wire::IncomingUsage;
use crate::loop_engine::transport::{
    CacheHint, ContentBlock, LoopTransport, Message, TransportError, TurnRequest, TurnResponse,
};
use crate::providers::http_body::{MAX_ERROR_BODY_BYTES, MAX_RESPONSE_BODY_BYTES};
use crate::providers::tests::http_fixture::{ENDLESS_STREAM_CEILING, FixtureResponse, serve};

#[test]
fn response_usage_retains_cached_reads_and_standard_cache_writes() {
    let usage: IncomingUsage = serde_json::from_value(serde_json::json!({
        "prompt_tokens": 1_000,
        "completion_tokens": 50,
        "prompt_tokens_details": {
            "cached_tokens": 200,
            "cache_write_tokens": 300
        }
    }))
    .expect("valid OpenAI-compatible usage");

    let mapped = turn_usage_from_wire(usage);
    assert_eq!(mapped.input_tokens, 1_000);
    assert_eq!(mapped.cache_read_input_tokens, 200);
    assert_eq!(mapped.cache_creation_input_tokens, 300);
    assert_eq!(mapped.output_tokens, 50);
}

#[test]
fn response_usage_accepts_cache_creation_alias() {
    let usage: IncomingUsage = serde_json::from_value(serde_json::json!({
        "prompt_tokens_details": {
            "cache_creation_tokens": 17
        }
    }))
    .expect("valid compatibility-layer usage");

    assert_eq!(turn_usage_from_wire(usage).cache_creation_input_tokens, 17);
}

#[test]
fn response_usage_accepts_a_top_level_cache_write_counter() {
    let usage: IncomingUsage = serde_json::from_value(serde_json::json!({
        "cache_creation_input_tokens": 23
    }))
    .expect("valid compatibility-layer usage");

    assert_eq!(turn_usage_from_wire(usage).cache_creation_input_tokens, 23);
}

fn send_turn_to(base_url: &str) -> Result<TurnResponse, TransportError> {
    let transport = OpenAiCompatTransport::new(
        base_url,
        "test-key",
        "gpt-test",
        Vec::<(String, String)>::new(),
    )
    .expect("transport");
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
        r#"{"choices":[{"finish_reason":"stop","message":{"content":"ok"}}],"usage":{"prompt_tokens":3,"completion_tokens":1}}"#,
    )]);

    let response = send_turn_to(&base_url).expect("send turn");
    let served = server.join().expect("server thread");

    assert!(
        served[0]
            .request_line
            .starts_with("POST /v1/chat/completions ")
    );
    assert!(matches!(&response.content[..], [ContentBlock::Text { text }] if text == "ok"));
    assert_eq!(response.usage.input_tokens, 3);
}

#[test]
fn oversized_chunked_success_is_refused_without_draining() {
    let (base_url, server) = serve(vec![FixtureResponse::endless(200)]);

    let err = send_turn_to(&base_url).expect_err("oversized body");
    let served = server.join().expect("server thread");

    let TransportError::Decode(message) = err else {
        panic!("expected oversized-body decode error, got {err}");
    };
    assert!(message.contains(&MAX_RESPONSE_BODY_BYTES.to_string()));
    assert!(served[0].body_bytes_written < ENDLESS_STREAM_CEILING);
}

#[test]
fn oversized_auth_failure_keeps_a_bounded_body() {
    let (base_url, server) = serve(vec![FixtureResponse::endless(401)]);

    let err = send_turn_to(&base_url).expect_err("auth failure");
    let served = server.join().expect("server thread");

    let TransportError::Auth(body) = err else {
        panic!("expected auth error, got {err}");
    };
    assert!(body.len() < MAX_ERROR_BODY_BYTES + 64);
    assert!(served[0].body_bytes_written < ENDLESS_STREAM_CEILING);
}
