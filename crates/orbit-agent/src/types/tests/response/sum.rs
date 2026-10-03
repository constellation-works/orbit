#![allow(missing_docs)]

use serde_json::json;

use orbit_types::telemetry::TokenUsage;

use super::super::super::response::usage::*;
#[test]
fn claude_model_usage_is_not_added_on_top_of_sibling_usage() {
    // Live shape from jrun-20260720-0150-3 (Claude `--output-format json`).
    // `usage` and `modelUsage` describe the same billed session; summing
    // both used to persist 2x cache_read (ORB-10906). The usage rollup
    // also carries the cache-creation TTL split that modelUsage lacks.
    let documents = vec![json!({
        "type": "result",
        "num_turns": 107,
        "usage": {
            "input_tokens": 212,
            "output_tokens": 46_418,
            "cache_read_input_tokens": 11_470_771,
            "cache_creation_input_tokens": 148_874,
            "cache_creation": {
                "ephemeral_5m_input_tokens": 51,
                "ephemeral_1h_input_tokens": 148_823,
            }
        },
        "modelUsage": {
            "claude-haiku-4-5-20251001": {
                "inputTokens": 3856,
                "outputTokens": 17,
                "cacheReadInputTokens": 0,
                "cacheCreationInputTokens": 0,
                "costUSD": 0.003941,
            },
            "claude-sonnet-5": {
                "inputTokens": 212,
                "outputTokens": 46_418,
                "cacheReadInputTokens": 11_470_771,
                "cacheCreationInputTokens": 148_874,
                "costUSD": 5.031381,
            }
        }
    })];

    assert_eq!(
        sum_usage(&documents),
        TokenUsage {
            input: 212,
            cache_read: 11_470_771,
            cache_create: 51,
            cache_create_1h: 148_823,
            output: 46_418,
        }
    );
}
