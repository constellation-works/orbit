use chrono::{DateTime, Utc};

use crate::model::pricing::{derive_cost_usd, normalize_token_usage, shipped_price_table};
use orbit_types::telemetry::TokenUsage;
use orbit_types::telemetry::{InputTokenBasis, PriceRow, cost_from_rows};

fn dt(rfc3339: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(rfc3339)
        .expect("valid rfc3339 fixture timestamp")
        .with_timezone(&Utc)
}

#[test]
fn shipped_price_table_parses_and_is_non_empty() {
    assert!(
        !shipped_price_table().is_empty(),
        "shipped model_prices.yaml should seed at least one row"
    );
}

#[test]
fn derives_cost_for_a_covered_model() {
    let usage = TokenUsage {
        input: 1_000_000,
        cache_read: 0,
        cache_create: 0,
        cache_create_1h: 0,
        output: 1_000_000,
    };
    // claude-opus-4-7: input $5/M + output $25/M (corrected from the stale
    // 15/75 training-prior seed).
    let cost = derive_cost_usd("claude-opus-4-7", dt("2026-06-01T00:00:00Z"), &usage)
        .expect("claude-opus-4-7 is priced in the shipped table");
    assert!((cost - 30.0).abs() < f64::EPSILON, "cost was {cost}");
}

#[test]
fn returns_none_for_an_unpriced_model() {
    assert_eq!(
        derive_cost_usd("some-unpriced-model", Utc::now(), &TokenUsage::default()),
        None
    );
}

#[test]
fn returns_none_before_the_effective_date() {
    let usage = TokenUsage {
        input: 1_000,
        ..TokenUsage::default()
    };
    assert_eq!(
        derive_cost_usd("claude-opus-4-7", dt("2020-01-01T00:00:00Z"), &usage),
        None
    );
}

// ─── Ground-truth validation (ORB-10338) ────────────────────────────────────
// Token splits and provider-reported costs are taken verbatim from real worker
// run 91d7ef01 (supervisor run record `modelUsage`). The derived cost must
// reproduce the provider figure, which is the strongest check that the shipped
// rates are correct — stronger than any pricing page.

#[test]
fn ground_truth_opus_4_8_1m_reproduces_provider_cost() {
    // claude-opus-4-8[1m]: {input 36, output 8265, cache_read 858526,
    // cache_write_1h 37795} → provider costUSD 1.0140179999999996.
    // The `[1m]` context-window suffix resolves to the base claude-opus-4-8 row.
    let usage = TokenUsage {
        input: 36,
        cache_read: 858_526,
        cache_create: 0,
        cache_create_1h: 37_795,
        output: 8_265,
    };
    let cost = derive_cost_usd("claude-opus-4-8[1m]", dt("2026-07-19T00:00:00Z"), &usage)
        .expect("claude-opus-4-8[1m] resolves to the base opus-4-8 row");
    assert!(
        (cost - 1.014_018).abs() < 1e-6,
        "derived cost was {cost}, expected ~1.014018"
    );
}

#[test]
fn ground_truth_haiku_4_5_reproduces_provider_cost() {
    // claude-haiku-4-5-20251001: {input 1338, output 21} → provider 0.001443.
    let usage = TokenUsage {
        input: 1_338,
        cache_read: 0,
        cache_create: 0,
        cache_create_1h: 0,
        output: 21,
    };
    let cost = derive_cost_usd(
        "claude-haiku-4-5-20251001",
        dt("2026-07-19T00:00:00Z"),
        &usage,
    )
    .expect("claude-haiku-4-5-20251001 is priced in the shipped table");
    assert!(
        (cost - 0.001_443).abs() < 1e-9,
        "derived cost was {cost}, expected 0.001443"
    );
}

#[test]
fn one_hour_and_five_minute_cache_writes_price_distinctly() {
    // 1M tokens of each cache-write TTL against opus-4-8: 5m = 6.25, 1h = 10.0.
    let five_minute = TokenUsage {
        cache_create: 1_000_000,
        ..TokenUsage::default()
    };
    let one_hour = TokenUsage {
        cache_create_1h: 1_000_000,
        ..TokenUsage::default()
    };
    let at = dt("2026-07-19T00:00:00Z");
    let cost_5m = derive_cost_usd("claude-opus-4-8", at, &five_minute).expect("priced");
    let cost_1h = derive_cost_usd("claude-opus-4-8", at, &one_hour).expect("priced");
    assert!((cost_5m - 6.25).abs() < f64::EPSILON, "5m was {cost_5m}");
    assert!((cost_1h - 10.0).abs() < f64::EPSILON, "1h was {cost_1h}");
}

#[test]
fn context_window_suffix_falls_back_to_the_base_row() {
    let usage = TokenUsage {
        input: 1_000_000,
        ..TokenUsage::default()
    };
    let at = dt("2026-07-19T00:00:00Z");
    let base = derive_cost_usd("claude-opus-4-8", at, &usage).expect("base priced");
    let suffixed = derive_cost_usd("claude-opus-4-8[1m]", at, &usage).expect("suffix priced");
    assert_eq!(base, suffixed, "the [1m] variant prices at the base rate");
    assert!((base - 5.0).abs() < f64::EPSILON, "input rate was {base}");
}

/// Coverage guard (ORB-10338): every model string the fleet actually runs must
/// resolve to a price row, so a newly-fielded model can't silently derive
/// `None`/zero cost. Keyed by the exact strings observed in the worker ledger
/// (`modelUsage` keys) and the crew config. When the fleet adds a model, add it
/// here and to `assets/model_prices.yaml` in the same change.
///
/// This is the cheap, curated form. The fuller guard — a check that scans the
/// live invocation store/ledger for any `model` with no covering row — is a
/// follow-up (it also has to contend with the upstream data-quality issue that
/// the store currently records unversioned crew aliases like `opus`/`sonnet`
/// alongside resolved version strings; those aliases are deliberately NOT
/// priced here).
#[test]
fn every_fleet_model_string_is_priced() {
    const FLEET_MODELS: &[&str] = &[
        "claude-opus-5",
        "claude-opus-4-8",
        "claude-opus-4-8[1m]",
        "claude-opus-4-7",
        "claude-sonnet-5",
        "claude-haiku-4-5-20251001",
        "claude-fable-5",
        "claude-fable-5-1",
        "gpt-5.6-sol",
        "gpt-5.6-terra",
        "gpt-5.6-luna",
        "gpt-6-astra",
        "gemini-3.5-flash",
        "gemini-3.8-flash",
        "grok-build",
        "grok-4.5",
        "grok-4.6",
    ];
    // A nonzero split so a zero-rate row would still yield Some (we assert
    // coverage, not a specific figure).
    let usage = TokenUsage {
        input: 1_000,
        output: 1_000,
        ..TokenUsage::default()
    };
    // 2026-09-03: must be on/after the newest effective_from in the table
    // (gpt-6-astra and gemini-3.8-flash) while still covering every other
    // open-ended row.
    let at = dt("2026-09-03T00:00:00Z");
    for model in FLEET_MODELS {
        assert!(
            derive_cost_usd(model, at, &usage).is_some(),
            "fleet model {model} has no covering price row in model_prices.yaml"
        );
    }
}

#[test]
fn fable_5_1_cache_reads_bill_at_a_fortieth_of_input() {
    // Official rates from platform.claude.com/docs/en/models/fable-5-1/overview
    // (released 2026-09-01): $10 input, $0.25 cache read (0.025x, not the
    // 0.1x every other Claude row uses), $12.50 5m write, $20 1h write, $50
    // output. 1M of each split → 10 + 0.25 + 12.5 + 20 + 50 = 92.75.
    let usage = TokenUsage {
        input: 1_000_000,
        cache_read: 1_000_000,
        cache_create: 1_000_000,
        cache_create_1h: 1_000_000,
        output: 1_000_000,
    };
    let at = dt("2026-09-01T00:00:00Z");
    let cost = derive_cost_usd("claude-fable-5-1", at, &usage)
        .expect("claude-fable-5-1 is priced in the shipped table");
    assert!((cost - 92.75).abs() < 1e-9, "cost was {cost}");

    // Fable 5 keeps its own 0.1x cache-read rate; the 5.1 row must not shadow it.
    let cache_only = TokenUsage {
        cache_read: 1_000_000,
        ..TokenUsage::default()
    };
    let legacy = derive_cost_usd("claude-fable-5", at, &cache_only).expect("priced");
    assert!(
        (legacy - 1.0).abs() < f64::EPSILON,
        "fable-5 cache read was {legacy}"
    );
    assert!(
        derive_cost_usd("claude-fable-5-1", dt("2026-08-31T23:59:59Z"), &usage).is_none(),
        "no fable-5-1 row before its release"
    );
}

#[test]
fn sonnet_5_introductory_rate_became_the_standard_rate() {
    // The 3/15 rise scheduled for 2026-09-01 was cancelled before it took
    // effect; 2/10 is the one open-ended rate on both sides of that date.
    let usage = TokenUsage {
        input: 1_000_000,
        output: 1_000_000,
        ..TokenUsage::default()
    };
    for at in [
        "2026-08-31T23:59:59Z",
        "2026-09-01T00:00:00Z",
        "2027-01-01T00:00:00Z",
    ] {
        let rows = covering_rows("claude-sonnet-5", dt(at));
        assert_eq!(rows.len(), 1, "exactly one sonnet-5 row covers {at}");
        let cost = derive_cost_usd("claude-sonnet-5", dt(at), &usage).expect("priced");
        assert!(
            (cost - 12.0).abs() < f64::EPSILON,
            "cost at {at} was {cost}"
        );
    }
}

fn flat_row(model: &str, effective_from: &str, input_per_million_usd: f64) -> PriceRow {
    PriceRow {
        model: model.to_string(),
        effective_from: dt(effective_from),
        effective_until: None,
        input_token_basis: InputTokenBasis::Exclusive,
        input_per_million_usd,
        cache_read_per_million_usd: 0.0,
        cache_create_per_million_usd: 0.0,
        cache_create_1h_per_million_usd: 0.0,
        output_per_million_usd: 0.0,
    }
}

fn covering_rows(model: &str, at: DateTime<Utc>) -> Vec<&'static PriceRow> {
    shipped_price_table()
        .iter()
        .filter(|row| {
            row.model == model
                && at >= row.effective_from
                && row.effective_until.is_none_or(|until| at < until)
        })
        .collect()
}

#[test]
fn gpt_5_6_rates_change_at_the_exclusive_july_30_boundary_without_overlap() {
    let historical_at = dt("2026-07-29T23:59:59Z");
    let current_at = dt("2026-07-30T00:00:00Z");
    let expected = [
        (
            "gpt-5.6-sol",
            [5.0, 0.5, 6.25, 30.0],
            [5.0, 0.5, 6.25, 30.0],
        ),
        (
            "gpt-5.6-terra",
            [2.5, 0.25, 3.125, 15.0],
            [2.0, 0.2, 2.5, 12.0],
        ),
        (
            "gpt-5.6-luna",
            [1.0, 0.1, 1.25, 6.0],
            [0.2, 0.02, 0.25, 1.2],
        ),
    ];

    for (model, historical, current) in expected {
        let historical_rows = covering_rows(model, historical_at);
        let current_rows = covering_rows(model, current_at);
        assert_eq!(historical_rows.len(), 1, "{model} historical coverage");
        assert_eq!(current_rows.len(), 1, "{model} current coverage");

        let rates = |row: &PriceRow| {
            [
                row.input_per_million_usd,
                row.cache_read_per_million_usd,
                row.cache_create_per_million_usd,
                row.output_per_million_usd,
            ]
        };
        assert_eq!(
            rates(historical_rows[0]),
            historical,
            "{model} historical rates"
        );
        assert_eq!(rates(current_rows[0]), current, "{model} current rates");
        assert_eq!(
            historical_rows[0].input_token_basis,
            InputTokenBasis::GrossIncludesCache
        );
        assert_eq!(
            current_rows[0].input_token_basis,
            InputTokenBasis::GrossIncludesCache
        );
    }
}

#[test]
fn gpt_6_astra_uses_official_standard_rates() {
    let rows = covering_rows("gpt-6-astra", dt("2026-09-03T00:00:00Z"));
    assert_eq!(rows.len(), 1, "astra has one current price row");
    let row = rows[0];
    assert_eq!(row.input_token_basis, InputTokenBasis::GrossIncludesCache);
    assert_eq!(row.input_per_million_usd, 10.0);
    assert_eq!(row.cache_read_per_million_usd, 1.0);
    assert_eq!(row.cache_create_per_million_usd, 12.5);
    assert_eq!(row.cache_create_1h_per_million_usd, 12.5);
    assert_eq!(row.output_per_million_usd, 50.0);
}

/// Gemini's `promptTokenCount` is the total effective prompt including the
/// cached content (ai.google.dev/api/generate-content, UsageMetadata), so the
/// cached share must come out of the input bucket before the input rate
/// applies; the old exclusive rows billed it twice.
#[test]
fn gemini_prompt_total_is_gross_of_cached_content() {
    let at = dt("2026-09-03T00:00:00Z");
    let usage = TokenUsage {
        input: 100_000,
        cache_read: 90_000,
        output: 1_000,
        ..TokenUsage::default()
    };
    // gemini-3.8-flash introductory: 10k uncached @ 0.75 + 90k cached @ 0.075
    // + 1k output @ 3.75 = 0.0075 + 0.00675 + 0.00375 = 0.018.
    let cost = derive_cost_usd("gemini-3.8-flash", at, &usage).expect("priced");
    assert!((cost - 0.018).abs() < 1e-9, "cost was {cost}");
    let normalized = normalize_token_usage("gemini-3.8-flash", at, &usage).expect("normalized");
    assert_eq!(normalized.input, 10_000);
    assert_eq!(normalized.cache_read, 90_000);
    for model in ["gemini-3.5-flash", "gemini-3.7-flash", "gemini-3.8-flash"] {
        for row in covering_rows(model, at) {
            assert_eq!(
                row.input_token_basis,
                InputTokenBasis::GrossIncludesCache,
                "{model}"
            );
        }
    }
}

#[test]
fn gross_openai_input_is_split_into_uncached_read_and_write_buckets() {
    let at = dt("2026-07-30T00:00:00Z");
    let cached = TokenUsage {
        input: 1_000_000,
        cache_read: 500_000,
        ..TokenUsage::default()
    };
    let cached_cost = derive_cost_usd("gpt-5.6-sol", at, &cached).expect("priced");
    assert!(
        (cached_cost - 2.75).abs() < f64::EPSILON,
        "cost was {cached_cost}"
    );

    let combined = TokenUsage {
        input: 1_000_000,
        cache_read: 200_000,
        cache_create: 300_000,
        ..TokenUsage::default()
    };
    let combined_cost = derive_cost_usd("gpt-5.6-sol", at, &combined).expect("priced");
    assert!(
        (combined_cost - 4.475).abs() < 1e-12,
        "cost was {combined_cost}"
    );
}

#[test]
fn normalization_keeps_every_cache_bucket_mutually_exclusive() {
    let gross = TokenUsage {
        input: 100,
        cache_read: 20,
        cache_create: 30,
        cache_create_1h: 10,
        output: 5,
    };
    let normalized = normalize_token_usage("gpt-5.6-sol", dt("2026-07-30T00:00:00Z"), &gross)
        .expect("covered gross model");
    assert_eq!(normalized.input, 40);
    assert_eq!(normalized.cache_read, 20);
    assert_eq!(normalized.cache_create, 30);
    assert_eq!(normalized.cache_create_1h, 10);
    assert_eq!(normalized.output, 5);

    let exclusive = normalize_token_usage("claude-opus-4-7", dt("2026-07-30T00:00:00Z"), &gross)
        .expect("covered exclusive model");
    assert_eq!(exclusive, gross);
    assert!(normalize_token_usage("unknown", dt("2026-07-30T00:00:00Z"), &gross).is_none());
}

#[test]
fn gross_openai_input_rejects_cache_detail_larger_than_the_total() {
    let invalid = TokenUsage {
        input: 100,
        cache_read: 60,
        cache_create: 41,
        ..TokenUsage::default()
    };
    assert_eq!(
        derive_cost_usd("gpt-5.6-sol", dt("2026-07-30T00:00:00Z"), &invalid),
        None
    );
}

#[test]
fn malformed_openai_one_hour_writes_are_not_priced_as_free() {
    let usage = TokenUsage {
        input: 1_000_000,
        cache_create_1h: 100_000,
        ..TokenUsage::default()
    };
    let cost = derive_cost_usd("gpt-5.6-sol", dt("2026-07-30T00:00:00Z"), &usage)
        .expect("nonzero fallback rate prices malformed 1h data");
    assert!((cost - 5.125).abs() < 1e-12, "cost was {cost}");
}

#[test]
fn ground_truth_grok_4_6_uses_official_short_context_rates() {
    // Official short-context rates retrieved 2026-08-14T03:45:16Z from
    // https://docs.x.ai/developers/models/grok-4.6 and
    // https://docs.x.ai/developers/pricing: $2.00 input / $0.50 cached /
    // $6.00 output per 1M. 1M of each split → 2.0 + 0.5 + 6.0 = 8.5.
    let usage = TokenUsage {
        input: 1_000_000,
        cache_read: 1_000_000,
        cache_create: 0,
        cache_create_1h: 0,
        output: 1_000_000,
    };
    let cost = derive_cost_usd("grok-4.6", dt("2026-08-14T00:00:00Z"), &usage)
        .expect("grok-4.6 is priced in the shipped table");
    assert!((cost - 8.5).abs() < f64::EPSILON, "cost was {cost}");
}

#[test]
fn grok_4_5_uses_official_short_context_rates() {
    // Official short-context rates retrieved 2026-08-14T03:45:16Z from
    // https://docs.x.ai/developers/models/grok-4.5 and
    // https://docs.x.ai/developers/pricing: $2.00 input / $0.30 cached /
    // $6.00 output per 1M. 1M of each split → 2.0 + 0.3 + 6.0 = 8.3.
    let usage = TokenUsage {
        input: 1_000_000,
        cache_read: 1_000_000,
        cache_create: 0,
        cache_create_1h: 0,
        output: 1_000_000,
    };
    let cost = derive_cost_usd("grok-4.5", dt("2026-08-14T00:00:00Z"), &usage)
        .expect("grok-4.5 is priced in the shipped table");
    assert!((cost - 8.3).abs() < f64::EPSILON, "cost was {cost}");
}

#[test]
fn grok_malformed_one_hour_writes_are_not_priced_as_free() {
    let usage = TokenUsage {
        input: 1_000_000,
        cache_create_1h: 100_000,
        ..TokenUsage::default()
    };
    let cost = derive_cost_usd("grok-4.6", dt("2026-08-14T00:00:00Z"), &usage)
        .expect("nonzero fallback rate prices malformed 1h data");
    assert!((cost - 2.2).abs() < 1e-12, "cost was {cost}");
}

#[test]
fn exclusive_input_rows_preserve_existing_non_openai_accounting() {
    let usage = TokenUsage {
        input: 1_000_000,
        cache_read: 500_000,
        ..TokenUsage::default()
    };
    let cost =
        derive_cost_usd("claude-opus-4-7", dt("2026-07-30T00:00:00Z"), &usage).expect("priced");
    assert!((cost - 5.25).abs() < f64::EPSILON, "cost was {cost}");
}

#[test]
fn picks_the_most_recent_covering_row_when_ranges_overlap() {
    let rows = vec![
        flat_row("dup-model", "2026-01-01T00:00:00Z", 1.0),
        flat_row("dup-model", "2026-06-01T00:00:00Z", 2.0),
    ];
    let usage = TokenUsage {
        input: 1_000_000,
        ..TokenUsage::default()
    };
    let cost = cost_from_rows(&rows, "dup-model", dt("2026-07-01T00:00:00Z"), &usage)
        .expect("one row covers");
    assert!((cost - 2.0).abs() < f64::EPSILON, "cost was {cost}");
}

#[test]
fn respects_an_exclusive_effective_until_boundary() {
    let mut closed = flat_row("closed-model", "2026-01-01T00:00:00Z", 1.0);
    closed.effective_until = Some(dt("2026-06-01T00:00:00Z"));
    let rows = vec![closed];
    let usage = TokenUsage {
        input: 1_000_000,
        ..TokenUsage::default()
    };

    assert!(
        cost_from_rows(&rows, "closed-model", dt("2026-06-01T00:00:00Z"), &usage).is_none(),
        "effective_until is exclusive"
    );
    assert!(cost_from_rows(&rows, "closed-model", dt("2026-05-31T23:59:59Z"), &usage).is_some());
}
