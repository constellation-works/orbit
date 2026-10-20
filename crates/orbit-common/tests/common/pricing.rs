//  Embedded model pricing through the public lookup boundary. This separate
//  area binary exercises the shipped table without process or state fixtures.

#[test]
fn sonnet_5_5_prices_exclusive_token_buckets_from_launch_onward() {
    let launch: DateTime<Utc> = "2026-09-28T00:00:00Z".parse().unwrap();
    let cache_read_change: DateTime<Utc> = "2026-10-07T00:00:00Z".parse().unwrap();
    let unchanged_rate_cases = [
        (
            TokenUsage {
                input: 1_000_000,
                ..Default::default()
            },
            2.0,
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
    ];
    let cache_read_only = TokenUsage {
        cache_read: 1_000_000,
        ..Default::default()
    };
    let historical_cases = [
        (cache_read_only.clone(), 0.2),
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
    let current_cases = [
        (cache_read_only, 0.1),
        (
            TokenUsage {
                input: 10_000,
                cache_read: 20_000,
                cache_create: 30_000,
                cache_create_1h: 40_000,
                output: 5_000,
            },
            0.307,
        ),
    ];

    assert_eq!(
        derive_cost_usd(
            "claude-sonnet-5-5",
            launch - chrono::Duration::seconds(1),
            &unchanged_rate_cases[0].0
        ),
        None,
        "launch pricing must not backdate costs"
    );
    let assert_costs = |at, cases: &[(TokenUsage, f64)]| {
        for (usage, expected) in cases {
            let actual = derive_cost_usd("claude-sonnet-5-5", at, usage)
                .expect("missing Sonnet 5.5 pricing previously left fleet invocations unpriced");
            assert!(
                (actual - expected).abs() < 1e-12,
                "Sonnet 5.5 fleet-cost regression: {usage:?} at {at}: expected {expected}, got {actual}"
            );
        }
    };

    assert_costs(launch, &unchanged_rate_cases);
    assert_costs(
        cache_read_change - chrono::Duration::seconds(1),
        &unchanged_rate_cases,
    );
    assert_costs(cache_read_change, &unchanged_rate_cases);
    assert_costs(
        cache_read_change - chrono::Duration::seconds(1),
        &historical_cases,
    );
    assert_costs(cache_read_change, &current_cases);
    assert_costs(
        "2027-01-01T00:00:00Z".parse().unwrap(),
        &unchanged_rate_cases,
    );
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
