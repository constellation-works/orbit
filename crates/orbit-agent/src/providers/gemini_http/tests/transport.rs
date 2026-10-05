use super::super::transport::{GEMINI_API_KEY_HEADER, GeminiHttpTransport};
use crate::loop_engine::transport::{
    CacheHint, ContentBlock, LoopTransport, Message, MessageRole, StopReason, TransportError,
    TurnRequest,
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

/// True once the headers and the full `content-length` body have arrived.
fn request_complete(request: &[u8]) -> bool {
    let Some(header_end) = request.windows(4).position(|window| window == b"\r\n\r\n") else {
        return false;
    };
    let headers = String::from_utf8_lossy(&request[..header_end]).to_ascii_lowercase();
    let content_length = headers
        .lines()
        .find_map(|line| line.strip_prefix("content-length:"))
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(0);
    request.len() >= header_end + 4 + content_length
}

fn request_body(captured_request: &str) -> serde_json::Value {
    let (_, body) = captured_request
        .split_once("\r\n\r\n")
        .expect("request body separator");
    serde_json::from_str(body).expect("request body json")
}

fn send(
    response_body: &'static str,
    messages: &[Message],
) -> (
    Result<crate::loop_engine::transport::TurnResponse, TransportError>,
    String,
) {
    let (base_url, server) = spawn_one_request_server(response_body);
    let transport = GeminiHttpTransport::new(GEMINI_API_KEY, "gemini-test", None)
        .expect("transport")
        .with_base_url(base_url);
    let req = TurnRequest {
        system: None,
        messages,
        tools: &[],
        cache_hint: CacheHint::None,
        max_response_tokens: 0,
    };
    let result = transport.send_turn(&req);
    (result, server.join().expect("server thread"))
}

#[test]
fn function_call_part_with_thought_signature_decodes_to_tool_use() {
    let body = r#"{"candidates":[{"content":{"role":"model","parts":[{"functionCall":{"name":"lookup","args":{"q":"x"}},"thoughtSignature":"sig"},{"executableCode":{"language":"PYTHON","code":"1"}}]},"finishReason":"STOP"}]}"#;
    let (result, _) = send(body, &[Message::user_text("hi")]);
    let response = result.expect("decodes");

    assert!(matches!(response.stop_reason, StopReason::ToolUse));
    assert_eq!(response.content.len(), 1, "unknown part kinds are skipped");
    let ContentBlock::ToolUse { name, input, .. } = &response.content[0] else {
        panic!("expected tool use, got {:?}", response.content[0]);
    };
    assert_eq!(name, "lookup");
    assert_eq!(input, &serde_json::json!({"q": "x"}));
}

#[test]
fn candidate_without_parts_decodes_to_empty_max_tokens_turn() {
    let body = r#"{"candidates":[{"content":{"role":"model"},"finishReason":"MAX_TOKENS"}]}"#;
    let (result, _) = send(body, &[Message::user_text("hi")]);
    let response = result.expect("decodes");

    assert!(response.content.is_empty());
    assert!(matches!(response.stop_reason, StopReason::MaxTokens));
}

#[test]
fn non_object_tool_output_is_wrapped_as_function_response_object() {
    let ok = r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"ok"}]},"finishReason":"STOP"}]}"#;
    let messages = [
        Message::user_text("go"),
        Message {
            role: MessageRole::Assistant,
            content: vec![
                ContentBlock::ToolUse {
                    id: "a".into(),
                    name: "list".into(),
                    input: serde_json::json!({}),
                },
                ContentBlock::ToolUse {
                    id: "b".into(),
                    name: "count".into(),
                    input: serde_json::json!({}),
                },
                ContentBlock::ToolUse {
                    id: "c".into(),
                    name: "obj".into(),
                    input: serde_json::json!({}),
                },
            ],
        },
        Message {
            role: MessageRole::User,
            content: ["[1,2]", "7", "{\"k\":1}"]
                .iter()
                .zip(["a", "b", "c"])
                .map(|(content, id)| ContentBlock::ToolResult {
                    tool_use_id: id.into(),
                    content: (*content).into(),
                    is_error: false,
                })
                .collect(),
        },
    ];
    let (result, captured) = send(ok, &messages);
    result.expect("send turn");

    let body = request_body(&captured);
    let responses: Vec<_> = body["contents"][2]["parts"]
        .as_array()
        .expect("parts")
        .iter()
        .map(|part| part["functionResponse"]["response"].clone())
        .collect();
    assert_eq!(
        responses,
        vec![
            serde_json::json!({"result": [1, 2]}),
            serde_json::json!({"result": 7}),
            serde_json::json!({"k": 1}),
        ]
    );
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
            if request_complete(&request_bytes) {
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
