use orbit_types::telemetry::TokenUsage;
use serde_json::Value;

use super::{
    JsonMap,
    protocol_schema::{RESPONSE_ENVELOPE_SCHEMA_VERSION, RESPONSE_ENVELOPE_STATUSES},
};

const USAGE_CHILD_KEYS: &[&str] = &[
    "usage",
    "token_usage",
    "tokenUsage",
    "tokens",
    "usageMetadata",
    "usage_metadata",
];

#[derive(Clone, Copy)]
enum UsageKeyMode {
    Standard,
    TokenBlock,
}

// Visible through the `response` module to sibling-layout tests.
pub(in crate::types) fn sum_usage(documents: &[Value]) -> TokenUsage {
    let mut usage = TokenUsage::default();
    for document in documents {
        collect_usage(document, &mut usage);
    }
    usage
}

// Provider metadata and assistant-declared usage have separate boundaries.
// Never search arbitrary payloads or decode strings as provider events: tools
// and quoted transcripts can contain exactly the same counter names.
fn collect_usage(value: &Value, usage: &mut TokenUsage) {
    match value {
        Value::Object(map) => {
            if is_payload(map) {
                return;
            }
            if collect_local_usage(map, usage) {
                return;
            }
            if is_response_envelope(map) {
                collect_result_usage(map, usage);
                return;
            }

            // These are provider frame containers, not conversation histories.
            for key in ["data", "message", "item", "stats"] {
                if let Some(child) = map.get(key).filter(|child| child.is_object()) {
                    collect_usage(child, usage);
                }
            }
            let has_model_token_usage = map
                .get("tokens")
                .and_then(Value::as_object)
                .and_then(|tokens| usage_from_map(tokens, UsageKeyMode::TokenBlock))
                .is_some();
            for key in ["models", "roles"] {
                if key == "roles" && has_model_token_usage {
                    continue;
                }
                if let Some(entries) = map.get(key).and_then(Value::as_object) {
                    for entry in entries.values() {
                        collect_usage(entry, usage);
                    }
                }
            }

            // Parse only complete declared-answer contracts, never arbitrary
            // JSON strings. Prefer structured_output over its result echo.
            for key in [
                "structured_output",
                "text",
                "result",
                "response",
                "final",
                "final_message",
                "output",
                "content",
            ] {
                if let Some(answer) = map.get(key)
                    && collect_declared_usage(answer, usage)
                {
                    break;
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_usage(item, usage);
            }
        }
        Value::String(_) => {
            collect_declared_usage(value, usage);
        }
        _ => {}
    }
}

fn is_payload(map: &JsonMap) -> bool {
    matches!(
        map.get("type").and_then(Value::as_str),
        Some(
            "tool_use"
                | "tool_call"
                | "tool_result"
                | "function_call"
                | "function_call_output"
                | "custom_tool_call"
                | "custom_tool_call_output"
                | "command_execution"
                | "mcp_tool_call"
                | "reasoning"
                | "thinking"
                | "user"
        )
    ) || matches!(
        map.get("role").and_then(Value::as_str),
        Some("tool" | "user" | "system")
    )
}

// Returns true when direct counters are the rollup for this object; callers
// must not descend into its detail breakdown and count it again.
fn collect_local_usage(map: &JsonMap, usage: &mut TokenUsage) -> bool {
    if let Some(found) = usage_from_map(map, UsageKeyMode::Standard) {
        add_usage(usage, found);
        return true;
    }
    for &key in USAGE_CHILD_KEYS {
        if let Some(mode) = usage_key_mode(key)
            && let Some(child) = map.get(key).and_then(Value::as_object)
            && let Some(found) = usage_from_map(child, mode)
        {
            add_usage(usage, found);
        }
    }
    // Claude's usage and modelUsage describe the same billed totals. Keep
    // the rollup, including its cache TTL split (ORB-10906 / F2026-08-031).
    if !sibling_usage_rollup_present(map)
        && let Some(models) = map.get("modelUsage").and_then(Value::as_object)
    {
        for model in models.values().filter_map(Value::as_object) {
            if let Some(found) = usage_from_map(model, UsageKeyMode::Standard) {
                add_usage(usage, found);
            }
        }
    }
    false
}

fn is_response_envelope(map: &JsonMap) -> bool {
    map.get("schemaVersion").and_then(Value::as_u64)
        == Some(u64::from(RESPONSE_ENVELOPE_SCHEMA_VERSION))
        && map
            .get("status")
            .and_then(Value::as_str)
            .is_some_and(|status| RESPONSE_ENVELOPE_STATUSES.contains(&status))
        && map.contains_key("result")
}

fn collect_result_usage(map: &JsonMap, usage: &mut TokenUsage) {
    // The result may declare its own usage, but its application data (including
    // quoted provider streams) is not invocation telemetry.
    if let Some(result) = map.get("result").and_then(Value::as_object)
        && !is_payload(result)
    {
        collect_local_usage(result, usage);
    }
}

fn collect_declared_usage(value: &Value, usage: &mut TokenUsage) -> bool {
    match value {
        Value::Object(map) if !is_payload(map) && is_response_envelope(map) => {
            if !collect_local_usage(map, usage) {
                collect_result_usage(map, usage);
            }
            true
        }
        Value::Object(map) if map.get("type").and_then(Value::as_str) == Some("text") => map
            .get("text")
            .is_some_and(|text| collect_declared_usage(text, usage)),
        Value::String(raw) => match serde_json::from_str::<Value>(raw) {
            // Do not recursively decode quoted strings or provider transcripts.
            Ok(Value::Object(map)) => collect_declared_usage(&Value::Object(map), usage),
            _ => false,
        },
        Value::Array(items) => items.iter().any(|item| collect_declared_usage(item, usage)),
        _ => false,
    }
}

fn add_usage(usage: &mut TokenUsage, found: TokenUsage) {
    usage.input = usage.input.saturating_add(found.input);
    usage.cache_read = usage.cache_read.saturating_add(found.cache_read);
    usage.cache_create = usage.cache_create.saturating_add(found.cache_create);
    usage.cache_create_1h = usage.cache_create_1h.saturating_add(found.cache_create_1h);
    usage.output = usage.output.saturating_add(found.output);
}

fn sibling_usage_rollup_present(map: &JsonMap) -> bool {
    USAGE_CHILD_KEYS.iter().any(|&key| {
        let Some(mode) = usage_key_mode(key) else {
            return false;
        };
        map.get(key)
            .and_then(Value::as_object)
            .and_then(|child| usage_from_map(child, mode))
            .is_some()
    })
}

fn usage_key_mode(key: &str) -> Option<UsageKeyMode> {
    match key {
        "tokens" => Some(UsageKeyMode::TokenBlock),
        "usage" | "token_usage" | "tokenUsage" | "usageMetadata" | "usage_metadata" => {
            Some(UsageKeyMode::Standard)
        }
        _ => None,
    }
}

fn usage_from_map(map: &JsonMap, key_mode: UsageKeyMode) -> Option<TokenUsage> {
    let input = match key_mode {
        UsageKeyMode::Standard => first_u64(map, STANDARD_INPUT_KEYS),
        UsageKeyMode::TokenBlock => first_u64(map, TOKEN_BLOCK_INPUT_KEYS),
    };
    let cache_read = match key_mode {
        UsageKeyMode::Standard => first_u64(map, STANDARD_CACHE_READ_KEYS),
        UsageKeyMode::TokenBlock => first_u64(map, TOKEN_BLOCK_CACHE_READ_KEYS),
    };
    let cache_create = match key_mode {
        UsageKeyMode::Standard => first_u64(map, STANDARD_CACHE_CREATE_KEYS),
        UsageKeyMode::TokenBlock => first_u64(map, STANDARD_CACHE_CREATE_KEYS),
    };
    // Gemini reports visible output and reasoning ("thoughts") as separate
    // counters, in the CLI token block and in `usageMetadata` alike
    // (`totalTokenCount` is prompt + thoughts + candidates); both consume the
    // output budget, so sum them rather than first-wins. `tool` is the small
    // tool-call channel of the token block and is also part of the output side.
    let output = match key_mode {
        UsageKeyMode::Standard => sum_present(&[
            first_u64(map, STANDARD_OUTPUT_KEYS),
            first_u64(map, THOUGHT_KEYS),
        ]),
        UsageKeyMode::TokenBlock => sum_present(&[
            first_u64(map, TOKEN_BLOCK_OUTPUT_KEYS),
            first_u64(map, THOUGHT_KEYS),
            first_u64(map, TOKEN_BLOCK_TOOL_KEYS),
        ]),
    };

    let cache_creation_split = match key_mode {
        UsageKeyMode::Standard => cache_creation_split(map),
        UsageKeyMode::TokenBlock => None,
    };

    if input.is_none()
        && cache_read.is_none()
        && cache_create.is_none()
        && cache_creation_split.is_none()
        && output.is_none()
    {
        return None;
    }

    let (cache_create, cache_create_1h) = match cache_creation_split {
        // Claude CLI reports the aggregate cache-creation count as well as a
        // TTL split. Prefer the split whenever it is present so 1h writes can
        // be priced at their premium rate rather than being folded into 5m.
        Some((five_minutes, one_hour)) => (five_minutes, one_hour),
        // Older Claude CLI output and providers without a TTL split retain the
        // historical aggregate-as-5m interpretation.
        None => (cache_create.unwrap_or(0), 0),
    };

    Some(TokenUsage {
        input: input.unwrap_or(0),
        cache_read: cache_read.unwrap_or(0),
        cache_create,
        cache_create_1h,
        output: output.unwrap_or(0),
    })
}

const STANDARD_INPUT_KEYS: &[&str] = &[
    "input_tokens",
    "inputTokens",
    "prompt_tokens",
    "promptTokens",
    "promptTokenCount",
    "prompt_token_count",
];

const TOKEN_BLOCK_INPUT_KEYS: &[&str] = &[
    "input_tokens",
    "inputTokens",
    "prompt_tokens",
    "promptTokens",
    "promptTokenCount",
    "prompt_token_count",
    "input",
    "prompt",
];

const STANDARD_CACHE_READ_KEYS: &[&str] = &[
    "cache_read_input_tokens",
    "cacheReadInputTokens",
    "cache_read_tokens",
    "cacheReadTokens",
    "cached_input_tokens",
    "cachedInputTokens",
    "cachedContentTokenCount",
    "cached_content_token_count",
];

const TOKEN_BLOCK_CACHE_READ_KEYS: &[&str] = &[
    "cache_read_input_tokens",
    "cacheReadInputTokens",
    "cache_read_tokens",
    "cacheReadTokens",
    "cached_input_tokens",
    "cachedInputTokens",
    "cachedContentTokenCount",
    "cached_content_token_count",
    "cached",
];

const STANDARD_CACHE_CREATE_KEYS: &[&str] = &[
    "cache_creation_input_tokens",
    "cacheCreationInputTokens",
    "cache_create_tokens",
    "cacheCreateTokens",
    "cache_write_input_tokens",
    "cacheWriteInputTokens",
    "cache_write_tokens",
    "cacheWriteTokens",
    "cache_creation_tokens",
    "cacheCreationTokens",
];

const CACHE_CREATION_KEY: &str = "cache_creation";
const EPHEMERAL_5M_INPUT_TOKENS_KEY: &str = "ephemeral_5m_input_tokens";
const EPHEMERAL_1H_INPUT_TOKENS_KEY: &str = "ephemeral_1h_input_tokens";

const STANDARD_OUTPUT_KEYS: &[&str] = &[
    "output_tokens",
    "outputTokens",
    "completion_tokens",
    "completionTokens",
    "candidatesTokenCount",
    "candidates_token_count",
];

const TOKEN_BLOCK_OUTPUT_KEYS: &[&str] = &[
    "output_tokens",
    "outputTokens",
    "completion_tokens",
    "completionTokens",
    "candidatesTokenCount",
    "candidates_token_count",
    "candidates",
    "output",
];

const THOUGHT_KEYS: &[&str] = &[
    "thoughts",
    "thoughtsTokenCount",
    "thoughts_token_count",
    "thinking_tokens",
    "thinkingTokens",
];

/// `None` when no counter was present; otherwise the saturating sum of the
/// ones that were.
fn sum_present(parts: &[Option<u64>]) -> Option<u64> {
    parts.iter().any(Option::is_some).then(|| {
        parts
            .iter()
            .flatten()
            .fold(0_u64, |total, value| total.saturating_add(*value))
    })
}

const TOKEN_BLOCK_TOOL_KEYS: &[&str] = &["tool", "toolTokenCount", "tool_token_count"];

/// Returns Claude CLI's cache-creation TTL split when either split field is
/// present. A missing counterpart is zero: the provider has explicitly chosen
/// the split format, so the aggregate is not a reliable 5m-only fallback.
fn cache_creation_split(map: &JsonMap) -> Option<(u64, u64)> {
    let cache_creation = map.get(CACHE_CREATION_KEY)?.as_object()?;
    let five_minutes = cache_creation
        .get(EPHEMERAL_5M_INPUT_TOKENS_KEY)
        .and_then(value_as_u64);
    let one_hour = cache_creation
        .get(EPHEMERAL_1H_INPUT_TOKENS_KEY)
        .and_then(value_as_u64);

    five_minutes
        .or(one_hour)
        .map(|_| (five_minutes.unwrap_or(0), one_hour.unwrap_or(0)))
}

fn first_u64(map: &JsonMap, keys: &[&str]) -> Option<u64> {
    keys.iter().find_map(|key| value_as_u64(map.get(*key)?))
}

pub(super) fn value_as_u64(value: &Value) -> Option<u64> {
    match value {
        Value::Number(number) => number.as_u64(),
        Value::String(raw) => raw.parse::<u64>().ok(),
        _ => None,
    }
}
