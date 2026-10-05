//! OpenAI-compatible chat-completions transport.
//!
//! The loop/session/audit/tool-dispatch mechanics live in the shared HTTP loop.
//! This module is deliberately only the wire-format adapter for request/response
//! mapping plus endpoint/header configuration.

use std::time::Duration;

use reqwest::blocking::Client;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderName, HeaderValue};
use serde_json::{Value, json};

use crate::loop_engine::transport::{
    ContentBlock, LoopTransport, Message, MessageRole, StopReason, ToolSpec, TransportError,
    TurnRequest, TurnResponse, TurnUsage,
};

use crate::providers::http_body::{read_error_body, read_response_body};

use super::wire::{
    ChatCompletionsRequest, ChatCompletionsResponse, FunctionDefinition, IncomingMessage,
    IncomingToolCall, IncomingUsage, OutgoingFunctionCall, OutgoingToolCall, RequestMessage,
    ToolDefinition,
};

const DEFAULT_BASE_URL: &str = "https://api.openai.com";
const DEFAULT_ENDPOINT_PATH: &str = "/v1/chat/completions";

pub struct OpenAiCompatTransport {
    client: Client,
    base_url: String,
    endpoint_path: String,
    api_key: String,
    model: String,
    custom_headers: Vec<(HeaderName, HeaderValue)>,
    send_bearer_auth: bool,
}

impl OpenAiCompatTransport {
    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
        custom_headers: Vec<(String, String)>,
    ) -> Result<Self, TransportError> {
        let client = build_client(Duration::from_secs(120))?;
        Ok(Self {
            client,
            base_url: normalize_base_url(base_url.into()),
            endpoint_path: DEFAULT_ENDPOINT_PATH.to_string(),
            api_key: api_key.into(),
            model: model.into(),
            custom_headers: validate_headers(custom_headers)?,
            send_bearer_auth: true,
        })
    }

    pub fn hosted(
        api_key: impl Into<String>,
        model: impl Into<String>,
    ) -> Result<Self, TransportError> {
        Self::new(
            DEFAULT_BASE_URL,
            api_key.into(),
            model.into(),
            Vec::<(String, String)>::new(),
        )
    }

    pub fn with_endpoint_path(mut self, endpoint_path: impl Into<String>) -> Self {
        self.endpoint_path = normalize_endpoint_path(endpoint_path.into());
        self
    }

    pub fn with_timeout(mut self, dur: Duration) -> Result<Self, TransportError> {
        self.client = build_client(dur)?;
        Ok(self)
    }

    pub fn with_bearer_auth(mut self, enabled: bool) -> Self {
        self.send_bearer_auth = enabled;
        self
    }

    pub fn endpoint(&self) -> String {
        format!("{}{}", self.base_url, self.endpoint_path)
    }
}

impl LoopTransport for OpenAiCompatTransport {
    fn provider(&self) -> &str {
        "openai_compat"
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn send_turn(&self, req: &TurnRequest<'_>) -> Result<TurnResponse, TransportError> {
        let wire_req = build_request(self.model.clone(), req);
        let body_bytes = serde_json::to_vec(&wire_req)
            .map_err(|e| TransportError::Decode(format!("serialize request: {e}")))?;

        let endpoint = self.endpoint();
        let mut request = self
            .client
            .post(&endpoint)
            .header(CONTENT_TYPE, "application/json");

        let has_custom_auth = self
            .custom_headers
            .iter()
            .any(|(name, _)| *name == AUTHORIZATION);
        if self.send_bearer_auth && !self.api_key.trim().is_empty() && !has_custom_auth {
            request = request.header(AUTHORIZATION, format!("Bearer {}", self.api_key));
        }

        for (name, value) in &self.custom_headers {
            request = request.header(name.clone(), value.clone());
        }

        let response = request
            .body(body_bytes.clone())
            .send()
            .map_err(|e| TransportError::Network(e.to_string()))?;

        let http_status = response.status().as_u16();
        if !(200..300).contains(&http_status) {
            let body = read_error_body(response)?;
            if matches!(http_status, 401 | 403) {
                return Err(TransportError::Auth(body));
            }
            return Err(TransportError::BadStatus {
                status: http_status,
                body,
            });
        }
        let response_bytes = read_response_body(response)?;

        let parsed: ChatCompletionsResponse = serde_json::from_slice(&response_bytes)
            .map_err(|e| TransportError::Decode(format!("parse response: {e}")))?;
        let choice =
            parsed.choices.into_iter().next().ok_or_else(|| {
                TransportError::Decode("response contained no choices".to_string())
            })?;

        let content = map_incoming_message(choice.message);
        let usage = turn_usage_from_wire(parsed.usage);

        let mut stop_reason = map_stop_reason(choice.finish_reason.as_deref());
        if content
            .iter()
            .any(|b| matches!(b, ContentBlock::ToolUse { .. }))
        {
            stop_reason = StopReason::ToolUse;
        }

        Ok(TurnResponse {
            content,
            stop_reason,
            usage,
            raw_request_body: body_bytes,
            raw_response_body: response_bytes,
            endpoint,
            http_status,
        })
    }
}

pub(super) fn turn_usage_from_wire(usage: IncomingUsage) -> TurnUsage {
    let details = usage.prompt_tokens_details.unwrap_or_default();
    TurnUsage {
        input_tokens: usage.prompt_tokens,
        output_tokens: usage.completion_tokens,
        cache_read_input_tokens: details.cached_tokens,
        // Compatibility layers use both placements. They may echo the same
        // aggregate in both, so prefer the larger value instead of summing.
        cache_creation_input_tokens: details
            .cache_write_tokens
            .max(usage.cache_creation_input_tokens),
    }
}

fn build_request(model: String, req: &TurnRequest<'_>) -> ChatCompletionsRequest {
    let mut messages = Vec::new();
    if let Some(system) = req.system {
        messages.push(RequestMessage {
            role: "system".to_string(),
            content: Some(system.to_string()),
            tool_calls: Vec::new(),
            tool_call_id: None,
        });
    }
    for message in req.messages {
        encode_message(message, &mut messages);
    }

    let tools = req.tools.iter().map(to_outgoing_tool).collect::<Vec<_>>();
    ChatCompletionsRequest {
        model,
        messages,
        max_tokens: req.max_response_tokens,
        tool_choice: (!tools.is_empty()).then(|| "auto".to_string()),
        tools,
    }
}

fn encode_message(message: &Message, out: &mut Vec<RequestMessage>) {
    match message.role {
        MessageRole::Assistant => encode_assistant_message(message, out),
        MessageRole::User => encode_user_message(message, out),
    }
}

fn encode_assistant_message(message: &Message, out: &mut Vec<RequestMessage>) {
    let mut text_parts = Vec::new();
    let mut tool_calls = Vec::new();

    for block in &message.content {
        match block {
            ContentBlock::Text { text } => text_parts.push(text.clone()),
            ContentBlock::ToolUse { id, name, input } => tool_calls.push(OutgoingToolCall {
                id: id.clone(),
                kind: "function",
                function: OutgoingFunctionCall {
                    name: name.clone(),
                    arguments: serde_json::to_string(input)
                        .unwrap_or_else(|_| "{\"error\":\"serialize\"}".to_string()),
                },
            }),
            ContentBlock::ToolResult { content, .. } => text_parts.push(content.clone()),
        }
    }

    if text_parts.is_empty() && tool_calls.is_empty() {
        return;
    }

    out.push(RequestMessage {
        role: "assistant".to_string(),
        content: (!text_parts.is_empty()).then(|| text_parts.join("\n")),
        tool_calls,
        tool_call_id: None,
    });
}

fn encode_user_message(message: &Message, out: &mut Vec<RequestMessage>) {
    let mut pending_text = Vec::new();

    for block in &message.content {
        match block {
            ContentBlock::Text { text } => pending_text.push(text.clone()),
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                ..
            } => {
                flush_user_text(&mut pending_text, out);
                out.push(RequestMessage {
                    role: "tool".to_string(),
                    content: Some(content.clone()),
                    tool_calls: Vec::new(),
                    tool_call_id: Some(tool_use_id.clone()),
                });
            }
            ContentBlock::ToolUse { name, input, .. } => pending_text.push(format!(
                "[unexpected tool request replayed as user text] {} {}",
                name, input
            )),
        }
    }

    flush_user_text(&mut pending_text, out);
}

fn flush_user_text(pending_text: &mut Vec<String>, out: &mut Vec<RequestMessage>) {
    if pending_text.is_empty() {
        return;
    }
    out.push(RequestMessage {
        role: "user".to_string(),
        content: Some(pending_text.join("\n")),
        tool_calls: Vec::new(),
        tool_call_id: None,
    });
    pending_text.clear();
}

fn to_outgoing_tool(spec: &ToolSpec) -> ToolDefinition {
    ToolDefinition {
        kind: "function",
        function: FunctionDefinition {
            name: spec.name.clone(),
            description: spec.description.clone(),
            parameters: if spec.input_schema.is_object() {
                spec.input_schema.clone()
            } else {
                json!({"type": "object", "properties": {}})
            },
        },
    }
}

fn map_stop_reason(raw: Option<&str>) -> StopReason {
    match raw {
        Some("stop") => StopReason::EndTurn,
        Some("tool_calls") => StopReason::ToolUse,
        Some("length") => StopReason::MaxTokens,
        _ => StopReason::Other,
    }
}

fn map_incoming_message(message: IncomingMessage) -> Vec<ContentBlock> {
    let mut content = Vec::new();
    let text = flatten_text_content(message.content.as_ref());
    if !text.is_empty() {
        content.push(ContentBlock::Text { text });
    }
    for (idx, tool_call) in message.tool_calls.into_iter().enumerate() {
        content.push(map_incoming_tool_call(tool_call, idx));
    }
    content
}

fn map_incoming_tool_call(tool_call: IncomingToolCall, idx: usize) -> ContentBlock {
    let id = if tool_call.id.is_empty() {
        format!("tool_call_{}", idx + 1)
    } else {
        tool_call.id
    };

    ContentBlock::ToolUse {
        id,
        name: tool_call.function.name,
        input: parse_function_arguments(&tool_call.function.arguments),
    }
}

fn flatten_text_content(value: Option<&Value>) -> String {
    let Some(value) = value else {
        return String::new();
    };

    match value {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(flatten_content_part)
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Object(map) => map
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        _ => value.to_string(),
    }
}

fn flatten_content_part(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Object(map) => {
            if let Some(text) = map.get("text").and_then(Value::as_str) {
                return Some(text.to_string());
            }
            map.get("content")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        }
        _ => None,
    }
}

fn parse_function_arguments(raw: &str) -> Value {
    match serde_json::from_str::<Value>(raw) {
        Ok(value) if value.is_object() || value.is_null() => value,
        Ok(value) => json!({ "value": value }),
        Err(_) if raw.trim().is_empty() => Value::Null,
        Err(_) => json!({ "raw_arguments": raw }),
    }
}

fn build_client(timeout: Duration) -> Result<Client, TransportError> {
    Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|e| TransportError::Other(format!("reqwest build: {e}")))
}

fn normalize_base_url(base_url: String) -> String {
    let trimmed = base_url.trim();
    let normalized = if trimmed.is_empty() {
        DEFAULT_BASE_URL
    } else {
        trimmed
    };
    normalized.trim_end_matches('/').to_string()
}

fn normalize_endpoint_path(path: String) -> String {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        DEFAULT_ENDPOINT_PATH.to_string()
    } else if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        format!("/{trimmed}")
    }
}

fn validate_headers(
    headers: Vec<(String, String)>,
) -> Result<Vec<(HeaderName, HeaderValue)>, TransportError> {
    headers
        .into_iter()
        .map(|(name, value)| {
            let header_name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|e| TransportError::Other(format!("invalid header name '{name}': {e}")))?;
            let header_value = HeaderValue::from_str(&value).map_err(|e| {
                TransportError::Other(format!("invalid header value for '{name}': {e}"))
            })?;
            Ok((header_name, header_value))
        })
        .collect()
}
