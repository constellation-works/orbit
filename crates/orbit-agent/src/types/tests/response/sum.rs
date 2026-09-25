#![allow(missing_docs)]

use serde_json::json;

use orbit_types::telemetry::TokenUsage;

use super::super::super::response::usage::*;

/// Gemini's `usageMetadata` reports reasoning beside the candidates;
/// `totalTokenCount` is prompt + thoughts + candidates, so both count as
/// output, and `promptTokenCount` stays the gross prompt total that the
/// price table's gross basis later splits.
#[test]
fn gemini_usage_metadata_counts_thoughts_as_output() {
    let documents = vec![json!({
        "usageMetadata": {
            "promptTokenCount": 100_000,
            "cachedContentTokenCount": 90_000,
            "candidatesTokenCount": 800,
            "thoughtsTokenCount": 4_200,
            "totalTokenCount": 105_000
        }
    })];

    assert_eq!(
        sum_usage(&documents),
        TokenUsage {
            input: 100_000,
            cache_read: 90_000,
            cache_create: 0,
            cache_create_1h: 0,
            output: 5_000,
        }
    );
}

#[test]
fn claude_cli_cache_creation_ttl_split_maps_each_ttl() {
    let documents = vec![json!({
        "usage": {
            "input_tokens": 36,
            "output_tokens": 8_265,
            "cache_read_input_tokens": 858_526,
            "cache_creation_input_tokens": 37_846,
            "cache_creation": {
                "ephemeral_5m_input_tokens": 51,
                "ephemeral_1h_input_tokens": 37_795,
            }
        }
    })];

    assert_eq!(
        sum_usage(&documents),
        TokenUsage {
            input: 36,
            cache_read: 858_526,
            cache_create: 51,
            cache_create_1h: 37_795,
            output: 8_265,
        }
    );
}

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

#[test]
fn claude_model_usage_is_collected_when_usage_rollup_is_absent() {
    let documents = vec![json!({
        "type": "result",
        "modelUsage": {
            "claude-haiku-4-5-20251001": {
                "inputTokens": 100,
                "outputTokens": 10,
                "cacheReadInputTokens": 20,
                "cacheCreationInputTokens": 5,
            },
            "claude-sonnet-5": {
                "inputTokens": 200,
                "outputTokens": 30,
                "cacheReadInputTokens": 400,
                "cacheCreationInputTokens": 15,
            }
        }
    })];

    assert_eq!(
        sum_usage(&documents),
        TokenUsage {
            input: 300,
            cache_read: 420,
            cache_create: 20,
            cache_create_1h: 0,
            output: 40,
        }
    );
}

#[test]
fn cache_creation_without_a_ttl_split_remains_five_minute_usage() {
    let documents = vec![json!({
        "usage": {
            "cache_creation_input_tokens": 100,
        }
    })];

    assert_eq!(
        sum_usage(&documents),
        TokenUsage {
            cache_create: 100,
            ..TokenUsage::default()
        }
    );
}

#[test]
fn gemini_cli_model_token_blocks_are_summed_once_per_model() {
    let documents = vec![json!({
        "stats": {
            "models": {
                "gemini-3.1-pro": {
                    "tokens": {
                        "input": 10,
                        "cached": 2,
                        "candidates": 4,
                        "total": 999,
                        "thoughts": 70,
                        "tool": 30
                    },
                    "roles": {
                        "user": {
                            "tokens": {
                                "input": 10,
                                "cached": 2
                            }
                        },
                        "model": {
                            "tokens": {
                                "candidates": 4
                            }
                        }
                    }
                },
                "gemini-2.5-flash": {
                    "tokens": {
                        "prompt": 20,
                        "cached": "3",
                        "output": "5",
                        "total": 28
                    },
                    "roles": {
                        "user": {
                            "tokens": {
                                "prompt": 20
                            }
                        },
                        "model": {
                            "tokens": {
                                "output": 5
                            }
                        }
                    }
                }
            }
        }
    })];

    assert_eq!(
        sum_usage(&documents),
        TokenUsage {
            input: 30,
            cache_read: 5,
            cache_create: 0,
            cache_create_1h: 0,
            output: 109,
        }
    );
}

#[test]
fn gemini_cli_role_tokens_are_counted_when_model_tokens_are_absent() {
    let documents = vec![json!({
        "stats": {
            "models": {
                "gemini-3.1-pro": {
                    "roles": {
                        "user": {
                            "tokens": {
                                "input": 7,
                                "cached": 1
                            }
                        },
                        "model": {
                            "tokens": {
                                "candidates": 3
                            }
                        }
                    }
                }
            }
        }
    })];

    assert_eq!(
        sum_usage(&documents),
        TokenUsage {
            input: 7,
            cache_read: 1,
            cache_create: 0,
            cache_create_1h: 0,
            output: 3,
        }
    );
}

#[test]
fn gemini_cli_thoughts_and_tool_are_folded_into_output() {
    let documents = vec![json!({
        "stats": {
            "models": {
                "gemini-3.1-pro": {
                    "tokens": {
                        "total": 999,
                        "thoughts": 70,
                        "tool": 30
                    }
                }
            }
        }
    })];

    // Gemini's `thoughts` and `tool` counters both consume the output
    // budget, so they sum into TokenUsage.output. `total` is a Gemini-side
    // rollup and is intentionally ignored to avoid double-counting.
    assert_eq!(
        sum_usage(&documents),
        TokenUsage {
            output: 100,
            ..TokenUsage::default()
        }
    );
}

#[test]
fn gemini_cli_live_turn_shape_sums_thoughts_into_output() {
    let documents = vec![json!({
        "stats": {
            "models": {
                "gemini-3.1-pro": {
                    "tokens": {
                        "input": 40919,
                        "output": 70,
                        "cached": 40101,
                        "thoughts": 396,
                        "tool": 0,
                        "total": 41385
                    }
                }
            }
        }
    })];

    assert_eq!(
        sum_usage(&documents),
        TokenUsage {
            input: 40919,
            cache_read: 40101,
            cache_create: 0,
            cache_create_1h: 0,
            output: 466,
        }
    );
}
