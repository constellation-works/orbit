use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;
use std::time::Duration;

use reqwest::blocking::Client;

use super::super::transport::{GEMINI_API_KEY_HEADER, GeminiHttpTransport, network_error};
use crate::loop_engine::transport::{
    CacheHint, LoopTransport, Message, TransportError, TurnRequest,
};

const GEMINI_API_KEY: &str = "AIzaSyDoNotLeakThisGeminiApiKeyValue";

#[test]
fn send_turn_sends_api_key_header_without_key_query_param() {
    let response_body = r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"ok"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":1,"candidatesTokenCount":1,"cachedContentTokenCount":0}}"#;
    let (base_url, server) = spawn_one_request_server(response_body);
    let transport = GeminiHttpTransport::new(GEMINI_API_KEY, "gemini-test", None)
        .expect("transport")
        .with_base_url(base_url);
    let messages = [Message::user_text("hello")];
    let req = TurnRequest {
        system: None,
        messages: &messages,
        tools: &[],
        cache_hint: CacheHint::None,
        max_response_tokens: 0,
    };

    let response = transport.send_turn(&req).expect("send turn");
    let captured_request = server.join().expect("server thread");
    let request_line = captured_request.lines().next().unwrap_or_default();

    assert!(
        request_line.starts_with("POST /v1beta/models/gemini-test:generateContent "),
        "unexpected request line: {request_line}"
    );
    assert!(
        !request_line.to_ascii_lowercase().contains("key="),
        "request URL must not contain an API key query param: {request_line}"
    );
    assert!(
        captured_request.contains(&format!("{GEMINI_API_KEY_HEADER}: {GEMINI_API_KEY}")),
        "request must send the Gemini API key header"
    );
    assert!(!response.endpoint.to_ascii_lowercase().contains("key="));
}

#[test]
fn send_turn_network_failures_use_a_secret_free_message() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind listener");
    let addr = listener.local_addr().expect("listener address");
    drop(listener);

    let transport = GeminiHttpTransport::new(GEMINI_API_KEY, "gemini-test", None)
        .expect("transport")
        .with_base_url(format!("http://{addr}"));
    let messages = [Message::user_text("hello")];
    let req = TurnRequest {
        system: None,
        messages: &messages,
        tools: &[],
        cache_hint: CacheHint::None,
        max_response_tokens: 0,
    };

    let TransportError::Network(message) = transport.send_turn(&req).expect_err("request fails")
    else {
        panic!("expected network error");
    };

    assert_eq!(message, "Gemini request failed");
    assert!(!message.contains(GEMINI_API_KEY));
}

#[test]
fn endpoint_builders_do_not_include_api_key_query_params() {
    let transport =
        GeminiHttpTransport::new(GEMINI_API_KEY, "gemini-test", None).expect("transport");

    let generate_endpoint = transport.generate_content_endpoint();
    let cached_endpoint = transport.cached_contents_endpoint();

    assert!(!generate_endpoint.to_ascii_lowercase().contains("key="));
    assert!(!cached_endpoint.to_ascii_lowercase().contains("key="));
    assert!(!generate_endpoint.contains(GEMINI_API_KEY));
    assert!(!cached_endpoint.contains(GEMINI_API_KEY));
}

#[test]
fn api_key_header_is_marked_sensitive_for_diagnostics() {
    let transport =
        GeminiHttpTransport::new(GEMINI_API_KEY, "gemini-test", None).expect("transport");

    let header_value = transport.api_key_header_for_test();

    assert!(header_value.is_sensitive());
    assert_eq!(format!("{header_value:?}"), "Sensitive");
    assert_eq!(header_value, GEMINI_API_KEY);
}

#[test]
fn reqwest_transport_errors_strip_url_before_stringifying() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind listener");
    let addr = listener.local_addr().expect("listener address");
    drop(listener);
    let url = format!("http://{addr}/fail?key={GEMINI_API_KEY}");
    let err = Client::builder()
        .timeout(Duration::from_millis(50))
        .build()
        .expect("client")
        .get(url)
        .send()
        .expect_err("unbound local port should fail");

    let TransportError::Network(message) = network_error(err) else {
        panic!("expected network error");
    };

    assert!(!message.contains(GEMINI_API_KEY));
    assert!(!message.to_ascii_lowercase().contains("key="));
}

fn spawn_one_request_server(response_body: &'static str) -> (String, thread::JoinHandle<String>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind listener");
    let addr = listener.local_addr().expect("listener address");
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept request");
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("set read timeout");
        let mut request_bytes = Vec::new();
        let mut buf = [0_u8; 1024];
        loop {
            let n = stream.read(&mut buf).expect("read request");
            if n == 0 {
                break;
            }
            request_bytes.extend_from_slice(&buf[..n]);
            if request_bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }

        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            response_body.len(),
            response_body
        );
        stream
            .write_all(response.as_bytes())
            .expect("write response");
        String::from_utf8(request_bytes).expect("request utf8")
    });

    (format!("http://{addr}"), handle)
}

/// Thinking models report reasoning tokens beside the visible candidates;
/// `totalTokenCount` is prompt + thoughts + candidates, so both are output.
#[test]
fn send_turn_counts_thought_tokens_as_output() {
    let response_body = r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"ok"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":100,"candidatesTokenCount":800,"thoughtsTokenCount":4200,"cachedContentTokenCount":90,"totalTokenCount":5100}}"#;
    let (base_url, server) = spawn_one_request_server(response_body);
    let transport = GeminiHttpTransport::new(GEMINI_API_KEY, "gemini-test", None)
        .expect("transport")
        .with_base_url(base_url);
    let messages = [Message::user_text("hello")];
    let req = TurnRequest {
        system: None,
        messages: &messages,
        tools: &[],
        cache_hint: CacheHint::None,
        max_response_tokens: 0,
    };

    let response = transport.send_turn(&req).expect("send turn");
    server.join().expect("server thread");

    assert_eq!(response.usage.input_tokens, 100);
    assert_eq!(response.usage.cache_read_input_tokens, 90);
    assert_eq!(response.usage.output_tokens, 5_000);
}
