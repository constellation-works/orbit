use orbit_types::telemetry::TokenUsage;
use serde_json::Value;

use super::JsonMap;

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

// Visible through `response.rs` to sibling-layout tests for the file-rooted
// response module.
pub(in crate::types) fn sum_usage(documents: &[Value]) -> TokenUsage {
    let mut usage = TokenUsage::default();
    for document in documents {
        collect_usage(document, &mut usage, true, UsageKeyMode::Standard);
    }
    usage
}

fn collect_usage(
    value: &Value,
    usage: &mut TokenUsage,
    allow_direct_usage: bool,
    key_mode: UsageKeyMode,
) {
    match value {
        Value::Object(map) => {
            if allow_direct_usage && let Some(found) = usage_from_map(map, key_mode) {
                add_usage(usage, found);
                return;
            }

            if matches!(map.get("type").and_then(Value::as_str), Some("tool_result")) {
                return;
            }

            let has_model_token_usage = map
                .get("tokens")
                .and_then(Value::as_object)
                .and_then(|tokens| usage_from_map(tokens, UsageKeyMode::TokenBlock))
                .is_some();
            // Claude CLI result docs put the same billed session totals on
            // `usage` and again on `modelUsage`. Summing both doubled
            // cache_read (ORB-10906 / F2026-08-031). Prefer the `usage`
            // rollup so the cache-creation TTL split is kept; skip the
            // sibling map. Sidecar-only tokens stay in `modelUsage` and
            // are still collected when no sibling rollup is present.
            let skip_model_usage = sibling_usage_rollup_present(map);

            for &key in USAGE_CHILD_KEYS {
                if let Some(mode) = usage_key_mode(key)
                    && let Some(child) = map.get(key)
                {
                    collect_usage(child, usage, true, mode);
                }
            }

            for (key, child) in map {
                if key != "tool_calls"
                    && usage_key_mode(key).is_none()
                    && !(has_model_token_usage && key == "roles")
                    && !(skip_model_usage && key == "modelUsage")
                {
                    let allow_child = allow_direct_usage
                        || matches!(
                            key.as_str(),
                            "text"
                                | "result"
                                | "response"
                                | "message"
                                | "messages"
                                | "content"
                                | "final"
                                | "final_message"
                                | "output"
                        );
                    collect_usage(child, usage, allow_child, UsageKeyMode::Standard);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_usage(item, usage, allow_direct_usage, key_mode);
            }
        }
        Value::String(raw) => {
            if allow_direct_usage && let Ok(nested) = serde_json::from_str::<Value>(raw) {
                collect_usage(&nested, usage, true, key_mode);
            }
        }
        _ => {}
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
