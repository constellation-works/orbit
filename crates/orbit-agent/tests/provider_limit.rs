//! Public provider timeout derivation and usage-limit reset instants, including
//! resets read across the year boundary.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::time::Duration;

use chrono::{FixedOffset, TimeZone, Utc};
use orbit_agent::{
    apply_antigravity_print_timeout, provider_usage_limit, provider_usage_limit_details,
    provider_usage_windows,
};

#[test]
fn january_reset_seen_in_late_december_holds_until_next_january() {
    let now = Utc.with_ymd_and_hms(2026, 12, 28, 10, 0, 0).unwrap();
    let text = "You've hit your limit · resets Jan 2, 3am (America/Los_Angeles)";

    let limit = provider_usage_limit_details(text, now, utc());

    // 3am PST on 2027-01-02 is 11:00 UTC.
    let expected = Utc.with_ymd_and_hms(2027, 1, 2, 11, 0, 0).unwrap();
    assert_eq!(limit.resets_at, Some(expected));
}

#[test]
fn explicit_reset_already_passed_today_rolls_to_tomorrow_not_next_year() {
    // 13:00 PDT on 2026-10-08; the 3am PDT reset passed ten hours ago.
    let now = Utc.with_ymd_and_hms(2026, 10, 8, 20, 0, 0).unwrap();
    let text = "You've hit your limit · resets Oct 8, 3am (America/Los_Angeles)";

    let limit = provider_usage_limit_details(text, now, utc());

    // 3am PDT on 2026-10-09 is 10:00 UTC: the same-day reset that has passed
    // is tomorrow's, and it must not be pushed a year ahead.
    let expected = Utc.with_ymd_and_hms(2026, 10, 9, 10, 0, 0).unwrap();
    assert_eq!(limit.resets_at, Some(expected));
}

#[test]
fn multibyte_text_before_the_zone_holds_by_default_without_panicking() {
    // `1é2` is four bytes, so the two-byte meridiem split lands inside `é`.
    let now = Utc.with_ymd_and_hms(2026, 10, 8, 20, 0, 0).unwrap();
    let text = "You've hit your limit · resets 1é2 (America/Los_Angeles)";

    assert!(provider_usage_limit(text));
    let limit = provider_usage_limit_details(text, now, utc());

    assert_eq!(limit.resets_at, None);
}

#[test]
fn oversized_relative_durations_do_not_panic_and_yield_no_reset() {
    let now = Utc.with_ymd_and_hms(2026, 10, 8, 20, 0, 0).unwrap();
    let cases = [
        // Out-of-range DateTime addition (overflows DateTime range)
        "Usage limit reached. Try again in 100000000 days.",
        // Out-of-range TimeDelta units (try_* returns None)
        "Usage limit reached. Resets in 99999999999999d",
        "Usage limit reached. Resets in 99999999999999 hours",
        "Usage limit reached. Resets in 999999999999999 minutes",
        "Usage limit reached. Resets in 9999999999999999 seconds",
        // Digits exceeding i64 range
        "Usage limit reached. Resets in 999999999999999999999999999999999999999999999999999999999999d",
        // Multi-unit duration whose sum overflows TimeDelta
        "Usage limit reached. Resets in 99999999999 days 99999999999 days",
    ];

    for text in cases {
        assert!(provider_usage_limit(text), "should detect limit in {text}");
        let limit = provider_usage_limit_details(text, now, utc());
        assert_eq!(
            limit.resets_at, None,
            "oversized duration in '{text}' should report no reset"
        );
    }
}

#[test]
fn well_formed_relative_resets_resolve_against_reference_time() {
    let now = Utc.with_ymd_and_hms(2026, 10, 8, 20, 0, 0).unwrap();

    let text1 = "Individual quota reached. Resets in 1h37m37s.";
    assert!(provider_usage_limit(text1));
    let limit1 = provider_usage_limit_details(text1, now, utc());
    let expected1 = Utc.with_ymd_and_hms(2026, 10, 8, 21, 37, 37).unwrap();
    assert_eq!(limit1.resets_at, Some(expected1));

    let text2 = "Usage limit reached. Try again in 5 minutes.";
    assert!(provider_usage_limit(text2));
    let limit2 = provider_usage_limit_details(text2, now, utc());
    let expected2 = Utc.with_ymd_and_hms(2026, 10, 8, 20, 5, 0).unwrap();
    assert_eq!(limit2.resets_at, Some(expected2));
}

#[test]
fn time_only_reset_across_autumn_dst_transition_preserves_local_clock() {
    // 16:00 PDT on 2026-10-31; 3pm PDT (22:00 UTC) has already passed.
    // Overnight on 2026-11-01, America/Los_Angeles falls back from PDT (UTC-7) to PST (UTC-8).
    let now = Utc.with_ymd_and_hms(2026, 10, 31, 23, 0, 0).unwrap();
    let text = "You've hit your limit · resets 3pm (America/Los_Angeles)";

    let limit = provider_usage_limit_details(text, now, utc());

    // Next-day 3pm PST on 2026-11-01 is 23:00 UTC (not 22:00 UTC).
    let expected = Utc.with_ymd_and_hms(2026, 11, 1, 23, 0, 0).unwrap();
    assert_eq!(limit.resets_at, Some(expected));
}

#[test]
fn time_only_reset_across_spring_dst_transition_preserves_local_clock() {
    // 15:00 PST on 2026-03-07; 3pm PST (23:00 UTC) has already passed.
    // Overnight on 2026-03-08, America/Los_Angeles springs forward from PST (UTC-8) to PDT (UTC-7).
    let now = Utc.with_ymd_and_hms(2026, 3, 7, 23, 0, 0).unwrap();
    let text = "You've hit your limit · resets 3pm (America/Los_Angeles)";

    let limit = provider_usage_limit_details(text, now, utc());

    // Next-day 3pm PDT on 2026-03-08 is 22:00 UTC (not 23:00 UTC).
    let expected = Utc.with_ymd_and_hms(2026, 3, 8, 22, 0, 0).unwrap();
    assert_eq!(limit.resets_at, Some(expected));
}

#[test]
fn nonexistent_or_invalid_reset_clocks_fail_safely_without_panicking() {
    // 2:30am does not exist on 2026-03-08 in America/Los_Angeles due to spring forward.
    let now = Utc.with_ymd_and_hms(2026, 3, 7, 23, 0, 0).unwrap();
    let nonexistent_next_day = "You've hit your limit · resets 2:30am (America/Los_Angeles)";
    assert!(provider_usage_limit(nonexistent_next_day));
    let limit = provider_usage_limit_details(nonexistent_next_day, now, utc());
    assert_eq!(limit.resets_at, None);

    // Explicit date nonexistent clock.
    let nonexistent_explicit = "You've hit your limit · resets Mar 8, 2:30am (America/Los_Angeles)";
    assert!(provider_usage_limit(nonexistent_explicit));
    let limit = provider_usage_limit_details(nonexistent_explicit, now, utc());
    assert_eq!(limit.resets_at, None);

    // Invalid clock formats (hour out of range 1..=12, missing meridiem).
    let invalid_clocks = [
        "You've hit your limit · resets 0am (America/Los_Angeles)",
        "You've hit your limit · resets 13pm (America/Los_Angeles)",
        "You've hit your limit · resets 3:60pm (America/Los_Angeles)",
        "You've hit your limit · resets 3 (America/Los_Angeles)",
    ];
    for text in invalid_clocks {
        assert!(provider_usage_limit(text));
        let limit = provider_usage_limit_details(text, now, utc());
        assert_eq!(limit.resets_at, None);
    }
}

fn utc() -> FixedOffset {
    FixedOffset::east_opt(0).unwrap()
}

fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| value.to_string()).collect()
}

#[test]
fn antigravity_reports_the_print_timeout_it_put_in_argv() {
    for provider in ["antigravity", "agy"] {
        let mut derived = args(&["--model", "m"]);
        let chosen =
            apply_antigravity_print_timeout(provider, &mut derived, Duration::from_secs(60));
        assert_eq!(chosen, Some(Duration::from_secs(30)));
        assert!(
            derived
                .windows(2)
                .any(|pair| pair == ["--print-timeout", "30s"])
        );

        // [ORB-15206] The budget is the deadline Orbit enforces minus the 30 s
        // margin, not a multiple of the runtime.
        let mut hour = args(&[]);
        let chosen =
            apply_antigravity_print_timeout(provider, &mut hour, Duration::from_secs(3600));
        assert_eq!(chosen, Some(Duration::from_secs(3570)));
        assert_eq!(hour, args(&["--print-timeout", "59m30s"]));

        for (deadline_secs, expected) in [(0, "1s"), (30, "30s"), (31, "1s"), (4200, "1h9m30s")] {
            let mut argv = Vec::new();
            apply_antigravity_print_timeout(
                provider,
                &mut argv,
                Duration::from_secs(deadline_secs),
            );
            assert_eq!(argv, args(&["--print-timeout", expected]));
        }
        let mut fractional = args(&["--print-timeout=1.5s"]);
        assert_eq!(
            apply_antigravity_print_timeout(provider, &mut fractional, Duration::from_secs(60)),
            Some(Duration::from_millis(1500)),
        );
        assert_eq!(fractional, args(&["--print-timeout=1.5s"]));

        let mut shorter = args(&["--print-timeout=10s"]);
        let chosen =
            apply_antigravity_print_timeout(provider, &mut shorter, Duration::from_secs(600));
        assert_eq!(
            chosen,
            Some(Duration::from_secs(10)),
            "a shorter custom value wins"
        );

        let mut longer = args(&["--print-timeout", "2h"]);
        let chosen =
            apply_antigravity_print_timeout(provider, &mut longer, Duration::from_secs(600));
        assert_eq!(
            chosen,
            Some(Duration::from_secs(570)),
            "a longer custom value is capped"
        );
        assert_eq!(longer, args(&["--print-timeout", "9m30s"]));
    }

    for provider in ["claude", "codex", "gemini", "grok"] {
        let mut argv = args(&["--model", "m"]);
        let chosen = apply_antigravity_print_timeout(provider, &mut argv, Duration::from_secs(600));
        assert_eq!(chosen, None, "{provider}");
        assert_eq!(
            argv,
            args(&["--model", "m"]),
            "{provider} argv must be untouched"
        );
    }
}

#[test]
fn grok_failure_texts_are_classified_as_provider_usage_limits() {
    let now = Utc.with_ymd_and_hms(2026, 10, 19, 14, 56, 0).unwrap();

    // Grok Build current 402 error text
    let grok_build_402 =
        "API error (status 402 Payment Required): Grok Build usage balance exhausted";
    assert!(
        provider_usage_limit(grok_build_402),
        "should detect limit in Grok Build 402 text"
    );
    let details = provider_usage_limit_details(grok_build_402, now, utc());
    assert_eq!(
        details.resets_at, None,
        "Grok Build 402 error specifies no reset time"
    );

    // Existing grok 1.0.46 texts
    let older_429 = "Error: Too Many Requests (429): rate limit exceeded for this account";
    assert!(
        provider_usage_limit(older_429),
        "should detect limit in grok 1.0.46 429 text"
    );
    let older_402 = "Error: Payment Required (402): account balance zero";
    assert!(
        provider_usage_limit(older_402),
        "should detect limit in grok 1.0.46 402 text"
    );
}

/// Muse Code 1.4.12 allowance payload recorded on macOS at 2026-10-23T04:13Z.
/// The fixture keeps that payload and minimal surrounding stream records.
const MUSE_USAGE: &[u8] = include_bytes!("provider_invocation/fixtures/muse_usage.jsonl");

#[test]
fn muse_run_usage_records_account_windows_at_completion() {
    let now = Utc.with_ymd_and_hms(2026, 10, 23, 4, 14, 0).unwrap();
    let readings = provider_usage_windows("muse", MUSE_USAGE, None, now);
    assert_eq!(readings.len(), 2, "the recorded allowance has two windows");
    for (reading, label, used, reset) in [
        (&readings[0], "window", 27.0, 1_792_740_094_000),
        (&readings[1], "weekly", 10.0, 1_792_972_800_000),
    ] {
        assert_eq!(reading.provider, "muse");
        assert_eq!(reading.window.as_deref(), Some(label));
        assert_eq!(reading.used_percent, Some(used));
        assert_eq!(
            reading.resets_at,
            chrono::DateTime::from_timestamp_millis(reset)
        );
        assert_eq!(
            reading.source,
            orbit_types::telemetry::ProviderLimitSource::Event
        );
        assert_eq!(reading.observed_at, now);
        assert!(reading.gating);
        assert!(!reading.exhausted);
        assert_eq!(reading.model, None);
        assert_eq!(reading.account, None);
        assert_eq!(reading.window_minutes, (label == "weekly").then_some(10080));
        assert_eq!(reading.run_id, None);
        assert_eq!(reading.crew, None);
    }
}

#[test]
fn muse_usage_windows_reject_absent_malformed_and_embedded_allowances() {
    use serde_json::{Value, json};
    let now = Utc::now();
    let records: Vec<Value> = std::str::from_utf8(MUSE_USAGE)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let status = records[1].clone();
    let mut cases = vec![
        json!({"payload_type": "status", "payload": {"kind": "run_usage", "models": []}}),
        json!({"payload_type": "status", "payload": {"kind": "other", "allowance": status["payload"]["allowance"]}}),
        json!({"schemaVersion": 1, "payload_type": "status", "payload": status["payload"]}),
    ];
    for record_type in [
        "run.lifecycle.started",
        "turn.input.user",
        "run.output.delta",
        "tool.result",
        "task.lifecycle.status",
        "run.terminal.completed",
    ] {
        cases.push(json!({"payload_type": record_type, "payload": status["payload"]}));
        cases.push(json!({"payload_type": record_type, "payload": {"text": status.to_string()}}));
    }
    for allowance in [
        Value::Null,
        json!([]),
        json!("unknown"),
        json!({"monthly": {"used_percent": 99, "resets_at_ms": 1792740094000_i64}}),
    ] {
        cases.push(json!({"payload_type": "status", "payload": {"kind": "run_usage", "allowance": allowance}}));
    }
    for fields in [
        Value::Null,
        json!([]),
        json!({}),
        json!({"used_percent": "27", "resets_at_ms": 1792740094000_i64}),
        json!({"used_percent": -1, "resets_at_ms": 1792740094000_i64}),
        json!({"used_percent": 27}),
        json!({"resets_at_ms": 1792740094000_i64}),
        json!({"used_percent": 27, "resets_at_ms": "1792740094000"}),
        json!({"used_percent": 27, "resets_at_ms": 1.5}),
        json!({"used_percent": 27, "resets_at_ms": i64::MAX}),
    ] {
        cases.push(json!({"payload_type": "status", "payload": {"kind": "run_usage", "allowance": {"window": fields, "weekly": fields}}}));
    }
    for record in cases {
        assert!(
            provider_usage_windows("muse", record.to_string().as_bytes(), None, now).is_empty(),
            "must ignore {record}"
        );
    }
    let no_allowance = format!("{}\n{}", records[0], records[2]);
    for capture in [b"".as_slice(), b"not json", no_allowance.as_bytes()] {
        assert!(provider_usage_windows("muse", capture, None, now).is_empty());
    }
    assert!(provider_usage_windows("claude", MUSE_USAGE, None, now).is_empty());
}

#[test]
fn muse_usage_windows_use_last_allowance_and_keep_valid_partial_windows() {
    use serde_json::{Value, json};
    let now = Utc::now();
    let mut status: Value =
        serde_json::from_slice::<Value>(MUSE_USAGE.split(|byte| *byte == b'\n').nth(1).unwrap())
            .unwrap();
    status["payload"]["allowance"] =
        json!({"weekly": {"used_percent": 102.125, "resets_at_ms": 1792972800000_i64}});
    let capture = format!("{}\n{status}", std::str::from_utf8(MUSE_USAGE).unwrap());
    let readings = provider_usage_windows("muse", capture.as_bytes(), None, now);
    assert_eq!(readings.len(), 1);
    assert_eq!(readings[0].window.as_deref(), Some("weekly"));
    assert_eq!(readings[0].used_percent, Some(102.125));
    assert!(readings[0].exhausted && readings[0].gating);
    status["payload"]["allowance"]["window"] = json!({"used_percent": "bad"});
    assert_eq!(
        provider_usage_windows("muse", status.to_string().as_bytes(), None, now),
        readings
    );
}
