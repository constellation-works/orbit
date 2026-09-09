//! Google Gemini `generateContent` HTTP transport.
//!
//! Uses the non-streaming REST API. If `cache_content_threshold_turns` is present
//! in the config and the history length matches or exceeds the threshold, it
//! automatically uses the `cachedContents` API to create a cache entry and
//! supplies the cache ID to `generateContent`.

use std::collections::HashMap;
use std::time::Duration;

use crate::loop_engine::transport::{
    ContentBlock, LoopTransport, Message, MessageRole, StopReason, ToolSpec, TransportError,
    TurnRequest, TurnResponse, TurnUsage,
};
use reqwest::blocking::{Client, RequestBuilder};
use reqwest::header::{CONTENT_TYPE, HeaderName, HeaderValue};

use super::wire::{
    Content, CreateCachedContentRequest, CreateCachedContentResponse, FunctionCall,
    FunctionDeclaration, FunctionResponse, GenerateContentRequest, GenerateContentResponse,
    GenerationConfig, Part, ToolDefinition,
};

const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com";
// `pub(super)` widened for sibling test access; otherwise file-private.
pub(super) const GEMINI_API_KEY_HEADER: &str = "x-goog-api-key";

pub struct GeminiHttpTransport {
    client: Client,
    base_url: String,
    api_key: String,
    model: String,
    cache_content_threshold_turns: Option<usize>,
    custom_headers: Vec<(HeaderName, HeaderValue)>,
}

impl GeminiHttpTransport {
    pub fn new(
        api_key: impl Into<String>,
        model: impl Into<String>,
        cache_content_threshold_turns: Option<usize>,
    ) -> Result<Self, TransportError> {
        let client = build_client(Duration::from_secs(120))?;
        Ok(Self {
            client,
            base_url: DEFAULT_BASE_URL.to_string(),
            api_key: api_key.into(),
            model: model.into(),
            cache_content_threshold_turns,
            custom_headers: Vec::new(),
        })
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = normalize_base_url(base_url.into());
        self
    }

    pub fn with_custom_headers(
        mut self,
        headers: Vec<(String, String)>,
    ) -> Result<Self, TransportError> {
        self.custom_headers = validate_headers(headers)?;
        Ok(self)
    }

    pub fn with_timeout(mut self, dur: Duration) -> Result<Self, TransportError> {
        self.client = build_client(dur)?;
        Ok(self)
    }

    // `pub(super)` widened for sibling test access.
    pub(super) fn generate_content_endpoint(&self) -> String {
        format!(
            "{}/v1beta/models/{}:generateContent",
            self.base_url, self.model
        )
    }

    // `pub(super)` widened for sibling test access.
    pub(super) fn cached_contents_endpoint(&self) -> String {
        format!("{}/v1beta/cachedContents", self.base_url)
    }

    fn post_json_request(&self, endpoint: &str) -> Result<RequestBuilder, TransportError> {
        let mut request = self
            .client
            .post(endpoint)
            .header(CONTENT_TYPE, "application/json")
            .header(GEMINI_API_KEY_HEADER, self.api_key_header_value()?);

        for (name, value) in &self.custom_headers {
            request = request.header(name.clone(), value.clone());
        }

        Ok(request)
    }

    // `pub(super)` widened for sibling test access.
    pub(super) fn api_key_header_value(&self) -> Result<HeaderValue, TransportError> {
        let mut header_value = HeaderValue::from_str(&self.api_key).map_err(|e| {
            TransportError::Other(format!("invalid Gemini API key header value: {e}"))
        })?;

        // RequestBuilder's debug output includes header values unless they are
        // explicitly marked sensitive. Keep the key usable on the wire while
        // preventing accidental cleartext disclosure in diagnostics.
        header_value.set_sensitive(true);
        Ok(header_value)
    }

    fn try_create_cached_content(
        &self,
        system_instruction: Option<Content>,
        contents: Vec<Content>,
        tools: Vec<ToolDefinition>,
    ) -> Result<Option<String>, TransportError> {
        let req = CreateCachedContentRequest {
            model: format!("models/{}", self.model),
            system_instruction,
            contents,
            tools,
            ttl: Some("3600s".to_string()),
        };

        let body_bytes = serde_json::to_vec(&req)
            .map_err(|e| TransportError::Decode(format!("serialize cache request: {e}")))?;

        let endpoint = self.cached_contents_endpoint();
        let request = self.post_json_request(&endpoint)?;

        let response = request.body(body_bytes).send().map_err(network_error)?;

        let http_status = response.status().as_u16();
        let response_bytes = response
            .bytes()
            .map_err(|e| network_error_with_context("read body", e))?
            .to_vec();

        if !(200..300).contains(&http_status) {
            let body = String::from_utf8_lossy(&response_bytes).to_string();
            // Failing to cache is an error that should bubble up to ensure
            // the transport behaves predictably.
            return Err(TransportError::BadStatus {
                status: http_status,
                body,
            });
        }

        let parsed: CreateCachedContentResponse = serde_json::from_slice(&response_bytes)
            .map_err(|e| TransportError::Decode(format!("parse cache response: {e}")))?;

        Ok(Some(parsed.name))
    }
}

impl LoopTransport for GeminiHttpTransport {
    fn provider(&self) -> &str {
        "gemini_http"
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn send_turn(&self, req: &TurnRequest<'_>) -> Result<TurnResponse, TransportError> {
        let system_instruction = req.system.map(|text| Content {
            role: "system".to_string(),
            parts: vec![Part::Text(text.to_string())],
        });

        let mut all_contents = Vec::new();
        let mut tool_names_by_id = HashMap::new();
        for message in req.messages {
            all_contents.push(encode_message(message, &mut tool_names_by_id));
        }

        let tools = if req.tools.is_empty() {
            Vec::new()
        } else {
            vec![ToolDefinition {
                function_declarations: req.tools.iter().map(to_function_declaration).collect(),
            }]
        };

        // Decide if we should cache
        let mut cached_content = None;
        let mut contents_to_send = all_contents.clone();

        if let Some(threshold) = self.cache_content_threshold_turns {
            // Only cache if we have multiple user/assistant turns matching or exceeding threshold
            let history_len = req.messages.len();
            if history_len >= threshold && history_len > 1 {
                // We cache all but the last message (which is the current user prompt)
                let c_contents = all_contents.drain(..history_len - 1).collect::<Vec<_>>();
                contents_to_send = all_contents; // The remaining message(s)

                cached_content = self.try_create_cached_content(
                    system_instruction.clone(),
                    c_contents,
                    tools.clone(),
                )?;
            }
        }

        // When cached_content is provided, system_instruction & tools cannot be passed
        // to generateContent again, as they are part of the cached content.
        let mut final_sys_instruction = system_instruction;
        let mut final_tools = tools;

        if cached_content.is_some() {
            final_sys_instruction = None;
            final_tools = Vec::new();
        }

        let wire_req = GenerateContentRequest {
            contents: contents_to_send,
            system_instruction: final_sys_instruction,
            tools: final_tools,
            generation_config: (req.max_response_tokens > 0).then_some(GenerationConfig {
                max_output_tokens: req.max_response_tokens,
            }),
            cached_content,
        };

        let body_bytes = serde_json::to_vec(&wire_req)
            .map_err(|e| TransportError::Decode(format!("serialize request: {e}")))?;

        let endpoint = self.generate_content_endpoint();
        let request = self.post_json_request(&endpoint)?;

        let response = request
            .body(body_bytes.clone())
            .send()
            .map_err(network_error)?;

        let http_status = response.status().as_u16();
        let response_bytes = response
            .bytes()
            .map_err(|e| network_error_with_context("read body", e))?
            .to_vec();

        if !(200..300).contains(&http_status) {
            let body = String::from_utf8_lossy(&response_bytes).to_string();
            if matches!(http_status, 401 | 403) {
                return Err(TransportError::Auth(body));
            }
            return Err(TransportError::BadStatus {
                status: http_status,
                body,
            });
        }

        let parsed: GenerateContentResponse =
            serde_json::from_slice(&response_bytes).map_err(|e| {
                TransportError::Decode(format!(
                    "parse response: {e}\nbody={}",
                    String::from_utf8_lossy(&response_bytes)
                ))
            })?;

        let candidate = parsed.candidates.into_iter().next().ok_or_else(|| {
            TransportError::Decode("response contained no candidates".to_string())
        })?;

        let content = candidate
            .content
            .map(map_incoming_content)
            .unwrap_or_default();

        let mut stop_reason = map_stop_reason(candidate.finish_reason.as_deref());
        if content
            .iter()
            .any(|b| matches!(b, ContentBlock::ToolUse { .. }))
        {
            stop_reason = StopReason::ToolUse;
        }

        let usage = TurnUsage {
            input_tokens: parsed.usage_metadata.prompt_token_count,
            output_tokens: parsed
                .usage_metadata
                .candidates_token_count
                .saturating_add(parsed.usage_metadata.thoughts_token_count),
            cache_read_input_tokens: parsed.usage_metadata.cached_content_token_count,
            cache_creation_input_tokens: 0,
        };

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

fn encode_message(message: &Message, tool_names_by_id: &mut HashMap<String, String>) -> Content {
    let role = match message.role {
        MessageRole::Assistant => "model",
        MessageRole::User => "user",
    };

    let parts = message
        .content
        .iter()
        .map(|block| match block {
            ContentBlock::Text { text } => Part::Text(text.clone()),
            ContentBlock::ToolUse { id, name, input } => {
                tool_names_by_id.insert(id.clone(), name.clone());
                Part::FunctionCall(FunctionCall {
                    id: Some(id.clone()),
                    name: name.clone(),
                    args: input.clone(),
                })
            }
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error: _,
            } => Part::FunctionResponse(FunctionResponse {
                id: Some(tool_use_id.clone()),
                name: tool_names_by_id
                    .get(tool_use_id)
                    .cloned()
                    .unwrap_or_else(|| {
                        tool_use_id
                            .split("::")
                            .next()
                            .unwrap_or("unknown_tool")
                            .to_string()
                    }),
                response: serde_json::from_str(content)
                    .unwrap_or_else(|_| serde_json::json!({ "result": content })),
            }),
        })
        .collect();

    Content {
        role: role.to_string(),
        parts,
    }
}

fn to_function_declaration(spec: &ToolSpec) -> FunctionDeclaration {
    FunctionDeclaration {
        name: spec.name.clone(),
        description: spec.description.clone(),
        parameters: Some(spec.input_schema.clone()),
    }
}

fn map_stop_reason(raw: Option<&str>) -> StopReason {
    match raw {
        Some("STOP") => StopReason::EndTurn,
        Some("MAX_TOKENS") => StopReason::MaxTokens,
        // Usually Gemini emits no specific finish reason for tool use but returns functionCalls.
        // It's also possible to see function calls alongside STOP.
        _ => StopReason::Other,
    }
}

fn map_incoming_content(content: Content) -> Vec<ContentBlock> {
    let mut blocks = Vec::new();
    for (idx, part) in content.parts.into_iter().enumerate() {
        match part {
            Part::Text(text) => blocks.push(ContentBlock::Text { text }),
            Part::FunctionCall(call) => blocks.push(ContentBlock::ToolUse {
                id: call.id.unwrap_or_else(|| format!("{}::{}", call.name, idx)),
                name: call.name,
                input: call.args,
            }),
            Part::FunctionResponse(_) => { /* Should not appear in incoming target responses normally */
            }
        }
    }

    blocks
}

fn build_client(timeout: Duration) -> Result<Client, TransportError> {
    Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|e| TransportError::Other(format!("reqwest build: {e}")))
}

// `pub(super)` widened for sibling test access.
pub(super) fn network_error(error: reqwest::Error) -> TransportError {
    TransportError::Network(reqwest_error_message(error))
}

fn network_error_with_context(context: &str, error: reqwest::Error) -> TransportError {
    TransportError::Network(format!("{context}: {}", reqwest_error_message(error)))
}

fn reqwest_error_message(error: reqwest::Error) -> String {
    error.without_url().to_string()
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
