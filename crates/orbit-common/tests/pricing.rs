//! Embedded model pricing through the public lookup boundary. This separate
//! area binary exercises the shipped table without process or state fixtures.
#![allow(missing_docs, clippy::expect_used, clippy::unwrap_used)]

orbit_common::isolate_test_process!();

use chrono::{DateTime, Utc};
use orbit_common::derive_cost_usd;
use orbit_types::telemetry::TokenUsage;

#[test]
fn sonnet_5_5_prices_exclusive_token_buckets_from_launch_onward() {
    let launch: DateTime<Utc> = "2026-09-28T00:00:00Z".parse().unwrap();
    let cases = [
        (
            TokenUsage {
                input: 1_000_000,
                ..Default::default()
            },
            2.0,
        ),
        (
            TokenUsage {
                cache_read: 1_000_000,
                ..Default::default()
            },
            0.2,
        ),
        (
            TokenUsage {
                cache_create: 1_000_000,
                ..Default::default()
            },
            2.5,
        ),
        (
            TokenUsage {
                cache_create_1h: 1_000_000,
                ..Default::default()
            },
            4.0,
        ),
        (
            TokenUsage {
                output: 1_000_000,
                ..Default::default()
            },
            10.0,
        ),
        // Input excludes cache buckets; subtracting them would underprice this.
        (
            TokenUsage {
                input: 10_000,
                cache_read: 20_000,
                cache_create: 30_000,
                cache_create_1h: 40_000,
                output: 5_000,
            },
            0.309,
        ),
    ];

    assert_eq!(
        derive_cost_usd(
            "claude-sonnet-5-5",
            launch - chrono::Duration::seconds(1),
            &cases[0].0
        ),
        None,
        "launch pricing must not backdate costs"
    );
    for at in [
        launch,
        "2026-10-03T01:12:07Z".parse().unwrap(),
        "2027-01-01T00:00:00Z".parse().unwrap(),
    ] {
        for (usage, expected) in &cases {
            let actual = derive_cost_usd("claude-sonnet-5-5", at, usage)
                .expect("missing Sonnet 5.5 pricing previously left fleet invocations unpriced");
            assert!(
                (actual - expected).abs() < 1e-12,
                "Sonnet 5.5 fleet-cost regression: {usage:?} at {at}: expected {expected}, got {actual}"
            );
        }
    }
}

#[test]
fn haiku_5_5_prices_short_prompt_rates_from_launch_onward() {
    let launch: DateTime<Utc> = "2026-10-07T00:00:00Z".parse().unwrap();
    // A real `claude -p --model claude-haiku-5-5` invocation on 2026-10-08
    // reported total_cost_usd 0.00268461 for these splits.
    let observed = TokenUsage {
        input: 2,
        cache_read: 10_481,
        cache_create: 0,
        cache_create_1h: 12_873,
        output: 10,
    };
    assert_eq!(
        derive_cost_usd(
            "claude-haiku-5-5",
            launch - chrono::Duration::seconds(1),
            &observed
        ),
        None,
        "launch pricing must not backdate costs"
    );
    let at: DateTime<Utc> = "2026-10-08T02:15:00Z".parse().unwrap();
    let actual = derive_cost_usd("claude-haiku-5-5", at, &observed)
        .expect("Haiku 5.5 runs the system crew and task pilot, so it must be priced");
    assert!(
        (actual - 0.002_684_61).abs() < 1e-12,
        "Haiku 5.5 cost must match the provider-reported total: got {actual}"
    );
}
