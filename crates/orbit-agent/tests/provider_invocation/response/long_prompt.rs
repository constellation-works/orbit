//! Recorded Claude Code 2.1.322 stream, 2026-11-10: 362,530 prompt tokens,
//! 3 starting versus 14,159 billed output tokens. Content and system frames
//! are omitted; the result answer is replaced with an Orbit envelope. Usage,
//! model, request IDs and message start/delta/stop ordering are unchanged.
use orbit_agent::provider_long_prompt_requests;
use orbit_types::telemetry::{
    InputTokenBasis, PriceRow, PromptTierRow, TokenUsage, cost_from_rows,
};
use serde_json::Value;

const STREAM: &str = include_str!("../fixtures/claude_final_usage.jsonl");
const MODEL: &str = "claude-haiku-5-5";

fn frames() -> Vec<Value> {
    STREAM
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn capture(frames: &[Value]) -> String {
    frames
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

fn usage() -> TokenUsage {
    TokenUsage {
        input: 2,
        cache_read: 2438,
        cache_create: 0,
        cache_create_1h: 360090,
        output: 14159,
    }
}

fn price(total: &TokenUsage, requests: Option<&[TokenUsage]>) -> Option<f64> {
    let at = "2026-11-10T00:00:00Z".parse().unwrap();
    // Distinct rates exercise all buckets without pinning the live price table.
    let base = PriceRow {
        model: MODEL.into(),
        effective_from: at,
        effective_until: None,
        input_token_basis: InputTokenBasis::Exclusive,
        input_per_million_usd: 1.0,
        cache_read_per_million_usd: 2.0,
        cache_create_per_million_usd: 3.0,
        cache_create_1h_per_million_usd: 4.0,
        output_per_million_usd: 5.0,
    };
    let tier = PromptTierRow {
        model: MODEL.into(),
        effective_from: at,
        effective_until: None,
        min_prompt_tokens: 100001,
        input_per_million_usd: 2.0,
        cache_read_per_million_usd: 4.0,
        cache_create_per_million_usd: 6.0,
        cache_create_1h_per_million_usd: 8.0,
        output_per_million_usd: 10.0,
    };
    cost_from_rows(&[base], &[tier], MODEL, at, total, requests)
}

#[test]
fn claude_final_usage_prices_recorded_requests_and_refuses_incomplete_evidence() {
    let now = chrono::Utc::now();
    let read = |raw: &str| {
        provider_long_prompt_requests("claude", raw.as_bytes(), true, None, Some(MODEL), now)
    };
    let expected = usage();
    let captured = read(STREAM);
    assert_eq!(
        captured,
        Some(vec![expected.clone()]),
        "ORB-23935: use final output, not the repeated starting count"
    );
    let weighted = (2.0 + 2438.0 * 2.0 + 360090.0 * 4.0 + 14159.0 * 5.0) / 1_000_000.0;
    assert!((price(&expected, captured.as_deref()).unwrap() - 2.0 * weighted).abs() < 1e-10);

    let original = frames();
    for missing in ["result", "message_start", "message_delta", "message_stop"] {
        let incomplete: Vec<_> = original
            .iter()
            .filter(|frame| frame["type"] != missing && frame["event"]["type"] != missing)
            .cloned()
            .collect();
        let requests = read(&capture(&incomplete));
        assert_eq!(requests, None, "missing {missing}");
        assert_eq!(price(&expected, requests.as_deref()), None);
    }
    let json_only = original.last().unwrap().to_string();
    assert_eq!(read(&json_only), None);
    assert_eq!(price(&expected, read(&json_only).as_deref()), None);

    let mut missing_cache_ttl_split = original.clone();
    for frame in &mut missing_cache_ttl_split {
        let usage = if frame["type"] == "assistant" {
            frame["message"]["usage"].as_object_mut()
        } else if frame["type"] == "stream_event" && frame["event"]["type"] == "message_start" {
            frame["event"]["message"]["usage"].as_object_mut()
        } else {
            None
        };
        if let Some(usage) = usage {
            usage.remove("cache_creation");
        }
    }
    let incomplete_cache_usage = read(&capture(&missing_cache_ttl_split));
    assert_eq!(incomplete_cache_usage, None, "missing cache TTL split");
    assert_eq!(
        price(&expected, incomplete_cache_usage.as_deref()),
        None,
        "unknown cache TTL buckets must not produce a derived tier price"
    );
    assert_eq!(
        provider_long_prompt_requests("claude", STREAM.as_bytes(), false, None, Some(MODEL), now),
        None
    );

    // Two real-shaped requests, repeated assistant blocks, an exact boundary,
    // and an interleaved different-model request with no final usage.
    let mut combined = original[..original.len() - 1].to_vec();
    let assistant = original
        .iter()
        .find(|frame| frame["type"] == "assistant")
        .unwrap();
    combined.push(assistant.clone());
    let mut totals = expected.clone();
    for (id, model, prompt) in [
        ("second", MODEL, 100001),
        ("boundary", MODEL, 100000),
        ("other", "claude-opus-5-5", 300000),
    ] {
        for mut frame in original[..original.len() - 1].iter().cloned() {
            if frame["type"] == "stream_event" {
                frame["api_message_id"] = id.into();
            }
            let message = if frame["type"] == "assistant" {
                &mut frame["message"]
            } else {
                &mut frame["event"]["message"]
            };
            if message.is_object() {
                message["id"] = id.into();
                message["model"] = model.into();
                message["usage"]["cache_creation_input_tokens"] = (prompt - 2440).into();
                message["usage"]["cache_creation"]["ephemeral_1h_input_tokens"] =
                    (prompt - 2440).into();
            }
            if frame["event"]["type"] == "message_delta" {
                frame["event"]["usage"]["cache_creation_input_tokens"] = (prompt - 2440).into();
            }
            if model != MODEL && frame["event"]["type"] == "message_delta" {
                continue;
            }
            combined.push(frame);
        }
        if model == MODEL {
            totals.input += 2;
            totals.cache_read += 2438;
            totals.cache_create_1h += prompt - 2440;
            totals.output += 14159;
        }
    }
    combined.push(original.last().unwrap().clone());
    let requests = read(&capture(&combined)).unwrap();
    assert_eq!(
        requests.len(),
        3,
        "deduplicate content blocks and exclude another model"
    );
    let base_cost = price(&totals, Some(&[])).unwrap();
    let tier_requests = [
        expected,
        TokenUsage {
            cache_create_1h: 97561,
            ..usage()
        },
    ];
    let premium = tier_requests
        .iter()
        .map(|request| price(request, Some(&[])).unwrap())
        .sum::<f64>();
    assert!((price(&totals, Some(&requests)).unwrap() - base_cost - premium).abs() < 1e-10);
    assert_eq!(
        provider_long_prompt_requests(
            "claude",
            capture(&combined).as_bytes(),
            true,
            None,
            Some("claude-haiku-5-5[1m]"),
            now
        ),
        Some(requests)
    );
}
