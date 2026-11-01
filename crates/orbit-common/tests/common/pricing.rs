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
            &unchanged_rate_cases[0].0,
            None
        ),
        None,
        "launch pricing must not backdate costs"
    );
    let assert_costs = |at, cases: &[(TokenUsage, f64)]| {
        for (usage, expected) in cases {
            let actual = derive_cost_usd("claude-sonnet-5-5", at, usage, None)
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
            &observed,
            None
        ),
        None,
        "launch pricing must not backdate costs"
    );
    let at: DateTime<Utc> = "2026-10-08T02:15:00Z".parse().unwrap();
    let after_tier: DateTime<Utc> = "2026-10-30T00:00:00Z".parse().unwrap();
    for at in [at, after_tier] {
        let actual = derive_cost_usd("claude-haiku-5-5", at, &observed, None)
            .expect("Haiku 5.5 runs the system crew and task pilot, so it must be priced");
        assert!(
            (actual - 0.002_684_61).abs() < 1e-12,
            "Haiku 5.5 cost must match the provider-reported total at {at}: got {actual}"
        );
    }
}

fn assert_cost(actual: Option<f64>, expected: f64, case: &str) {
    let actual = actual.unwrap_or_else(|| panic!("{case}: expected {expected}, got no cost"));
    assert!(
        (actual - expected).abs() < 1e-12,
        "{case}: expected {expected}, got {actual}"
    );
}

// [ORB-19680] Haiku 5.5 bills a request whose own prompt (input plus cache
// reads and writes) is over 100,000 tokens at its higher rates for the whole
// request. Pricing invocation totals at short-context rates derived about a
// third of the provider-reported Haiku spend.
#[test]
fn haiku_5_5_prices_each_request_over_100k_prompt_tokens_at_the_tier() {
    let at: DateTime<Utc> = "2026-10-30T00:00:00Z".parse().unwrap();
    // A run of several requests; the trace keeps the three at or over the
    // 100,000-token capture floor.
    let totals = TokenUsage {
        input: 30,
        cache_read: 450_000,
        cache_create: 60_000,
        cache_create_1h: 0,
        output: 4_000,
    };
    let long_requests = [
        // Prompt 105,005: over the threshold.
        TokenUsage {
            input: 5,
            cache_read: 95_000,
            cache_create: 10_000,
            cache_create_1h: 0,
            output: 800,
        },
        // Prompt 107,005: over the threshold, mostly cache hits.
        TokenUsage {
            input: 5,
            cache_read: 105_000,
            cache_create: 2_000,
            cache_create_1h: 0,
            output: 700,
        },
        // Prompt exactly 100,000: not over it, so standard rates.
        TokenUsage {
            input: 10,
            cache_read: 99_990,
            cache_create: 0,
            cache_create_1h: 0,
            output: 100,
        },
    ];
    // Totals at standard rates (0.014003) plus each tiered request's tier
    // minus standard cost (0.010402 and 0.006602).
    assert_cost(
        derive_cost_usd("claude-haiku-5-5", at, &totals, Some(&long_requests)),
        0.031_007,
        "tiered Haiku 5.5 run",
    );
    assert_cost(
        derive_cost_usd("claude-haiku-5-5", at, &totals, Some(&[])),
        0.014_003,
        "Haiku 5.5 run whose requests all stayed at or under 100,000 tokens",
    );
    assert_eq!(
        derive_cost_usd("claude-haiku-5-5", at, &totals, None),
        None,
        "totals over the threshold without per-request usage must stay unpriced, never estimated"
    );
    assert_cost(
        derive_cost_usd(
            "claude-haiku-5-5",
            "2026-10-28T00:00:00Z".parse().unwrap(),
            &totals,
            None,
        ),
        0.014_003,
        "an invocation before the tier's observation boundary keeps its shipped pricing",
    );
}

// [ORB-19680] OpenAI bills a request with more than 272K input tokens at 2x
// input and cache rates and 1.5x output for the whole request. Its input
// total is gross: cached reads are part of the prompt.
#[test]
fn openai_prices_a_request_over_272k_input_tokens_at_the_long_context_tier() {
    let at: DateTime<Utc> = "2026-10-30T00:00:00Z".parse().unwrap();
    // Two requests: a 300K-input one the trace kept, and a 50K-input one
    // under the capture floor.
    let long_request = TokenUsage {
        input: 300_000,
        cache_read: 250_000,
        output: 2_000,
        ..TokenUsage::default()
    };
    let totals = TokenUsage {
        input: 350_000,
        cache_read: 290_000,
        output: 3_000,
        ..TokenUsage::default()
    };
    // Totals at short-context rates (0.179) plus the long request's
    // long-context (0.28) minus short-context (0.145) cost.
    assert_cost(
        derive_cost_usd(
            "gpt-6.1-sol",
            at,
            &totals,
            Some(std::slice::from_ref(&long_request)),
        ),
        0.314,
        "gpt-6.1-sol request over 272K input tokens",
    );
    let at_threshold = TokenUsage {
        input: 272_000,
        ..long_request.clone()
    };
    assert_cost(
        derive_cost_usd(
            "gpt-6.1-sol",
            at,
            &TokenUsage {
                input: 322_000,
                ..totals.clone()
            },
            Some(std::slice::from_ref(&at_threshold)),
        ),
        0.123,
        "a 272,000-input-token request is short context",
    );
    assert_eq!(
        derive_cost_usd("gpt-6.1-sol", at, &totals, None),
        None,
        "totals over 272K without per-request usage must stay unpriced, never estimated"
    );
}

// Reviewer decision (2026-11-01): OpenAI's 2026-08-21 changelog cut prices
// gpt-5.6-sol at 4/20 with cache 0.4/5.0 from 2026-08-21T00:00:00Z. Before the
// cut the July 30 row keeps its shipped rates, the open row takes over at its
// observation boundary, and no invocation in between may be unpriced.
#[test]
fn gpt_5_6_sol_prices_the_august_21_cut_without_an_unpriced_gap() {
    let cut: DateTime<Utc> = "2026-08-21T00:00:00Z".parse().unwrap();
    let before_cut = cut - chrono::Duration::seconds(1);
    // Gross input: cache reads and both cache writes are inside `input`, so
    // the exclusive input is 10,000 tokens.
    let mixed = TokenUsage {
        input: 100_000,
        cache_read: 20_000,
        cache_create: 30_000,
        cache_create_1h: 40_000,
        output: 5_000,
    };
    // (usage, shipped July 30 rates, August 21 cut rates)
    let cases = [
        (
            TokenUsage {
                input: 1_000_000,
                ..Default::default()
            },
            5.0,
            4.0,
        ),
        (
            TokenUsage {
                input: 1_000_000,
                cache_read: 1_000_000,
                ..Default::default()
            },
            0.5,
            0.4,
        ),
        (
            TokenUsage {
                input: 1_000_000,
                cache_create: 1_000_000,
                ..Default::default()
            },
            6.25,
            5.0,
        ),
        (
            TokenUsage {
                output: 1_000_000,
                ..Default::default()
            },
            30.0,
            20.0,
        ),
        (mixed.clone(), 0.6475, 0.498),
    ];
    for (usage, shipped, cut_rate) in &cases {
        assert_cost(
            derive_cost_usd("gpt-5.6-sol", before_cut, usage, None),
            *shipped,
            &format!("gpt-5.6-sol on 2026-08-20 for {usage:?}"),
        );
        assert_cost(
            derive_cost_usd("gpt-5.6-sol", cut, usage, None),
            *cut_rate,
            &format!("gpt-5.6-sol from 2026-08-21 for {usage:?}"),
        );
    }
    // Every instant from the cut to the open row's boundary prices at the cut
    // rates, including 2026-09-22T00:00:00Z, which the shipped table left
    // unpriced, and the last microsecond before the open row.
    for at in [
        "2026-09-01T12:00:00Z",
        "2026-09-21T23:59:59Z",
        "2026-09-22T00:00:00Z",
        "2026-10-22T12:00:00Z",
        "2026-10-23T03:09:14.111506Z",
        "2026-10-23T03:09:14.111507Z",
    ] {
        let at: DateTime<Utc> = at.parse().unwrap();
        assert_cost(
            derive_cost_usd("gpt-5.6-sol", at, &mixed, None),
            0.498,
            &format!("gpt-5.6-sol at {at} must not be unpriced"),
        );
    }
}
