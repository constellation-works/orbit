use super::super::transport::{GEMINI_API_KEY_HEADER, GeminiHttpTransport};
use crate::loop_engine::transport::{
    CacheHint, LoopTransport, Message, TransportError, TurnRequest,
};

use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;
use std::time::Duration;

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
