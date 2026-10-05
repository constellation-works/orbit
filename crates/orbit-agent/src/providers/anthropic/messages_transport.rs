//! Anthropic Messages API transport.
//!
//! `POST https://api.anthropic.com/v1/messages` via the blocking reqwest
//! client. Applies `cache_control` ephemeral markers to the last system block
//! and — per the loop's cache hint — the last message in the replayed
//! history, so the prefix up through the prior turn becomes cacheable on
//! subsequent sends.

use std::time::Duration;

use reqwest::blocking::Client;
use serde_json::{Value, json};

use crate::loop_engine::transport::{
    CacheHint, ContentBlock, LoopTransport, Message, MessageRole, StopReason, ToolSpec,
    TransportError, TurnRequest, TurnResponse, TurnUsage,
};

use crate::providers::http_body::{read_error_body, read_response_body};

use super::wire::{
    CacheControl, IncomingContent, MessagesRequest, MessagesResponse, OutgoingContent,
    OutgoingMessage, OutgoingTool, SystemBlock,
};

const DEFAULT_ENDPOINT: &str = "https://api.anthropic.com/v1/messages";
const DEFAULT_API_VERSION: &str = "2023-06-01";

pub struct AnthropicMessagesTransport {
    client: Client,
    api_key: String,
    model: String,
    endpoint: String,
    anthropic_version: String,
}

impl AnthropicMessagesTransport {
    pub fn new(
        api_key: impl Into<String>,
        model: impl Into<String>,
    ) -> Result<Self, TransportError> {
        let client = Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .map_err(|e| TransportError::Other(format!("reqwest build: {e}")))?;
        Ok(Self {
            client,
            api_key: api_key.into(),
            model: model.into(),
            endpoint: DEFAULT_ENDPOINT.to_string(),
            anthropic_version: DEFAULT_API_VERSION.to_string(),
        })
    }

    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = endpoint.into();
        self
    }

    pub fn with_timeout(mut self, dur: Duration) -> Result<Self, TransportError> {
        self.client = Client::builder()
            .timeout(dur)
            .build()
            .map_err(|e| TransportError::Other(format!("reqwest build: {e}")))?;
        Ok(self)
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }
}

impl LoopTransport for AnthropicMessagesTransport {
    fn provider(&self) -> &str {
        "anthropic"
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn send_turn(&self, req: &TurnRequest<'_>) -> Result<TurnResponse, TransportError> {
        if self.api_key.trim().is_empty() {
            return Err(TransportError::Auth(
                "missing Anthropic API key (ANTHROPIC_API_KEY)".to_string(),
            ));
        }

        let wire_req = build_request(self.model.clone(), req);
        let body_bytes = serde_json::to_vec(&wire_req)
            .map_err(|e| TransportError::Decode(format!("serialize request: {e}")))?;

        let response = self
            .client
            .post(&self.endpoint)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", &self.anthropic_version)
            .header("content-type", "application/json")
            .body(body_bytes.clone())
            .send()
            .map_err(|e| TransportError::Network(e.to_string()))?;

        let http_status = response.status().as_u16();
        if !(200..300).contains(&http_status) {
            return Err(TransportError::BadStatus {
                status: http_status,
                body: read_error_body(response)?,
            });
        }
        let response_bytes = read_response_body(response)?;

        let parsed: MessagesResponse = serde_json::from_slice(&response_bytes)
            .map_err(|e| TransportError::Decode(format!("parse response: {e}")))?;

        let stop_reason = map_stop_reason(parsed.stop_reason.as_deref());
        let content = parsed
            .content
            .into_iter()
            .filter_map(map_incoming_content)
            .collect();
        let usage = TurnUsage {
            input_tokens: parsed.usage.input_tokens,
            output_tokens: parsed.usage.output_tokens,
            cache_read_input_tokens: parsed.usage.cache_read_input_tokens,
            cache_creation_input_tokens: parsed.usage.cache_creation_input_tokens,
        };

        Ok(TurnResponse {
            content,
            stop_reason,
            usage,
            raw_request_body: body_bytes,
            raw_response_body: response_bytes,
            endpoint: self.endpoint.clone(),
            http_status,
        })
    }
}

fn build_request(model: String, req: &TurnRequest<'_>) -> MessagesRequest {
    let system = req.system.map(|text| {
        vec![SystemBlock {
            kind: "text",
            text: text.to_string(),
            cache_control: match req.cache_hint {
                CacheHint::None => None,
                CacheHint::SystemOnly | CacheHint::SystemAndEarliestHistory => {
                    Some(CacheControl::ephemeral())
                }
            },
        }]
    });

    let mut messages = Vec::with_capacity(req.messages.len());
    let last_idx = req.messages.len().saturating_sub(1);
    for (idx, m) in req.messages.iter().enumerate() {
        let mark_this_message = matches!(req.cache_hint, CacheHint::SystemAndEarliestHistory)
            && idx == last_idx
            && !req.messages.is_empty();
        messages.push(to_outgoing(m, mark_this_message));
    }

    let tools = req.tools.iter().map(to_outgoing_tool).collect();

    MessagesRequest {
        model,
        max_tokens: req.max_response_tokens,
        system,
        messages,
        tools,
    }
}

fn to_outgoing(msg: &Message, mark_last_block: bool) -> OutgoingMessage {
    let role = match msg.role {
        MessageRole::User => "user".to_string(),
        MessageRole::Assistant => "assistant".to_string(),
    };
    let count = msg.content.len();
    let content = msg
        .content
        .iter()
        .enumerate()
        .map(|(i, block)| {
            let apply_cache = mark_last_block && i + 1 == count;
            match block {
                ContentBlock::Text { text } => OutgoingContent::Text {
                    text: text.clone(),
                    cache_control: apply_cache.then(CacheControl::ephemeral),
                },
                ContentBlock::ToolUse { id, name, input } => OutgoingContent::ToolUse {
                    id: id.clone(),
                    name: name.clone(),
                    input: input.clone(),
                    cache_control: apply_cache.then(CacheControl::ephemeral),
                },
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } => OutgoingContent::ToolResult {
                    tool_use_id: tool_use_id.clone(),
                    content: content.clone(),
                    is_error: Some(*is_error),
                    cache_control: apply_cache.then(CacheControl::ephemeral),
                },
            }
        })
        .collect();
    OutgoingMessage { role, content }
}

fn to_outgoing_tool(spec: &ToolSpec) -> OutgoingTool {
    OutgoingTool {
        name: spec.name.clone(),
        description: spec.description.clone(),
        input_schema: if spec.input_schema.is_object() {
            spec.input_schema.clone()
        } else {
            json!({"type": "object", "properties": {}})
        },
    }
}

fn map_stop_reason(raw: Option<&str>) -> StopReason {
    match raw {
        Some("end_turn") => StopReason::EndTurn,
        Some("tool_use") => StopReason::ToolUse,
        Some("max_tokens") => StopReason::MaxTokens,
        _ => StopReason::Other,
    }
}

fn map_incoming_content(block: IncomingContent) -> Option<ContentBlock> {
    match block {
        IncomingContent::Text { text } => Some(ContentBlock::Text { text }),
        IncomingContent::ToolUse { id, name, input } => Some(ContentBlock::ToolUse {
            id,
            name,
            input: coerce_input(input),
        }),
        IncomingContent::Unknown => None,
    }
}

fn coerce_input(value: Value) -> Value {
    if value.is_object() || value.is_null() {
        value
    } else {
        json!({ "value": value })
    }
}
