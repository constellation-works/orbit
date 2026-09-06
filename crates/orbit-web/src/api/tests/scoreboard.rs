// ORB-00337: window-aware scoreboard endpoint contract.
//
// Asserts the HTTP surface for `?window=` honors the scoreboard windowing
// behavior added in orbit-store / orbit-core:
// - missing param defaults to lifetime (`window: "all"`)
// - `?window=1h` round-trips into the serialized payload + populates
//   `window_since`
// - unknown values produce HTTP 400 (not a 500)
// - schema_version is the v9 value (notable completions + coverage) with its
//   separately-versioned managed-execution orchestration section

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use chrono::{DateTime, Duration, Utc};
use orbit_core::OrbitRuntime;
use serde_json::json;
use tower::ServiceExt;

use super::super::incidents::ActorFailureRollup;
use super::super::scoreboard::{
    MetricsExtras, apply_side_source_extras, compute_metrics_extras, months_in_range,
};
use super::super::*;
use super::test_support::{body_json, write_lines};

async fn get_scoreboard(runtime: OrbitRuntime, query: Option<&str>) -> axum::response::Response {
    let uri = match query {
        Some(q) => format!("/scoreboard?{q}"),
        None => "/scoreboard".to_string(),
    };
    router()
        .with_state(crate::state::DashboardState::single(Arc::new(runtime)))
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(uri)
                .body(Body::empty())
                .expect("build request"),
        )
        .await
        .expect("response")
}

#[tokio::test]
async fn scoreboard_default_returns_lifetime_window_and_v9_schema() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let response = get_scoreboard(runtime, None).await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["schema_version"].as_u64(), Some(9));
    assert_eq!(body["window"].as_str(), Some("all"));
    assert_eq!(
        body["coverage"]["review"]["availability"].as_str(),
        Some("observed")
    );
    assert_eq!(
        body["notable_completions"]["method"].as_str(),
        Some("priority_then_completion_recency")
    );
    assert!(
        body["notable_completions"]["label"]
            .as_str()
            .expect("selection label")
            .contains("not a quality score")
    );
    assert!(body["notable_completions"]["items"].as_array().is_some());
    assert!(
        body["window_since"].is_null(),
        "window_since is null for lifetime, got {:?}",
        body["window_since"]
    );
    assert!(body["orchestration"]["previous_normalized_tokens"].is_null());
    assert_eq!(body["orchestration"]["schema_version"].as_u64(), Some(2));
    assert_eq!(
        body["orchestration"]["normalized_tokens"]["normalized_token_total"].as_u64(),
        Some(0)
    );
    assert_eq!(body["orchestration"]["scope"], "managed_execution");
    assert!(
        chrono::DateTime::parse_from_rfc3339(
            body["orchestration"]["until"]
                .as_str()
                .expect("until timestamp"),
        )
        .expect("parse until")
            <= chrono::DateTime::parse_from_rfc3339(
                body["orchestration"]["as_of"]
                    .as_str()
                    .expect("as_of timestamp"),
            )
            .expect("parse as_of")
    );
    assert!(body["orchestration"]["buckets"].is_array());
}

#[tokio::test]
async fn scoreboard_query_window_1h_populates_window_and_since() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let response = get_scoreboard(runtime, Some("window=1h")).await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["schema_version"].as_u64(), Some(9));
    assert_eq!(body["window"].as_str(), Some("1h"));
    assert_eq!(
        body["coverage"]["review"]["availability"].as_str(),
        Some("unavailable")
    );
    assert!(
        body["coverage"]["review"]["detail"]
            .as_str()
            .expect("coverage detail")
            .contains("omitted from this window")
    );
    assert!(body["orchestration"]["previous_normalized_tokens"].is_object());
    let since = body["window_since"]
        .as_str()
        .expect("window_since is RFC3339 string for non-all window");
    // Surface check: parses as a RFC3339 timestamp.
    let _ =
        chrono::DateTime::parse_from_rfc3339(since).expect("window_since must be valid RFC3339");
    assert_eq!(
        chrono::DateTime::parse_from_rfc3339(
            body["orchestration"]["since"]
                .as_str()
                .expect("orchestration since"),
        )
        .expect("parse orchestration since"),
        chrono::DateTime::parse_from_rfc3339(since).expect("parse scoreboard since"),
    );
    assert!(
        chrono::DateTime::parse_from_rfc3339(
            body["orchestration"]["until"]
                .as_str()
                .expect("until timestamp"),
        )
        .expect("parse until")
            <= chrono::DateTime::parse_from_rfc3339(
                body["orchestration"]["as_of"]
                    .as_str()
                    .expect("as_of timestamp"),
            )
            .expect("parse as_of")
    );
}

#[tokio::test]
async fn scoreboard_query_window_bogus_returns_400_with_error_body() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let response = get_scoreboard(runtime, Some("window=bogus")).await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = body_json(response).await;
    let err = body["error"]
        .as_str()
        .expect("400 body has an 'error' string field");
    assert!(
        err.contains("bogus"),
        "error message names the bad input, got {err}"
    );
}

#[tokio::test]
async fn scoreboard_query_window_7d_is_not_a_24h_payload() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let response = get_scoreboard(runtime, Some("window=7d")).await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["window"].as_str(), Some("7d"));
    assert_ne!(
        body["window"].as_str(),
        Some("24h"),
        "a 7d request must not report a 24h window"
    );
    let since = chrono::DateTime::parse_from_rfc3339(
        body["window_since"].as_str().expect("window_since for 7d"),
    )
    .expect("parse window_since");
    let orch_since = chrono::DateTime::parse_from_rfc3339(
        body["orchestration"]["since"]
            .as_str()
            .expect("orchestration since"),
    )
    .expect("parse orchestration since");
    let until = chrono::DateTime::parse_from_rfc3339(
        body["orchestration"]["until"]
            .as_str()
            .expect("orchestration until"),
    )
    .expect("parse until");
    assert_eq!(
        since, orch_since,
        "scoreboard and managed-execution cutoffs must match"
    );
    let span = until.signed_duration_since(orch_since);
    assert!(
        span >= chrono::Duration::days(7) - chrono::Duration::seconds(2)
            && span <= chrono::Duration::days(7) + chrono::Duration::seconds(2),
        "7d orchestration span must be ~7 days, got {span}"
    );
    assert!(
        until
            <= chrono::DateTime::parse_from_rfc3339(
                body["orchestration"]["as_of"].as_str().expect("as_of"),
            )
            .expect("parse as_of")
    );
}

#[tokio::test]
async fn scoreboard_query_window_all_round_trips_explicitly() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let response = get_scoreboard(runtime, Some("window=all")).await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["window"].as_str(), Some("all"));
    assert!(body["window_since"].is_null());
}

/// ORB-10871: the per-agent row carries the grouped incident count next to the
/// raw `failed_tool_calls` it collapsed, scoped to the requested window, so the
/// leaderboard cannot present one burst as many quality failures.
#[tokio::test]
async fn scoreboard_reports_failure_incidents_beside_raw_failed_tool_calls() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    for index in 0..9 {
        runtime
            .record_audit_event(&orbit_core::AuditEventInsertParams {
                execution_id: format!("exec-{index}"),
                command: "tool".to_string(),
                subcommand: Some("run".to_string()),
                tool_name: Some("orbit.surface.alpha".to_string()),
                target_type: Some("tool".to_string()),
                target_id: Some("orbit.surface.alpha".to_string()),
                role: "claude".to_string(),
                status: orbit_core::AuditEventStatus::Failure,
                exit_code: 1,
                duration_ms: 3,
                working_directory: "/tmp/fixture".to_string(),
                arguments_json: None,
                stdout_truncated: None,
                stderr_truncated: None,
                error_message: Some(format!("could not remove /work/dir-{index}/file.txt")),
                host: None,
                pid: std::process::id(),
                session_id: None,
                workspace_id: None,
                caller_machine_id: None,
                caller_host_id: None,
                process_machine_id: None,
                process_host_id: None,
                transport: None,
                effective_capabilities: Default::default(),
                origin_session_id: None,
                mcp_call_id: None,
                lease_id: None,
                task_id: None,
                job_run_id: None,
                activity_id: None,
                step_index: None,
            })
            .expect("seed audit event");
    }

    let response = get_scoreboard(runtime, Some("window=24h")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;

    let agent = &body["agents"]["claude"];
    assert_eq!(
        agent["failed_tool_calls"].as_u64(),
        Some(9),
        "the raw failed-call count is preserved"
    );
    assert_eq!(
        agent["failure_incidents"].as_u64(),
        Some(1),
        "nine identical failures are one incident"
    );
    assert_eq!(
        agent["failure_incident_events"].as_u64(),
        Some(9),
        "the incident states how much raw evidence it collapsed"
    );
    assert_eq!(agent["unexpected_failure_incidents"].as_u64(), Some(1));
}

// ORB-11200: `compute_metrics_extras` must scope retries/avg/p95 to the
// scoreboard's own requested window instead of a fixed "current + prior
// month" guess, and `months_in_range` must walk actual calendar months so an
// early-month `now` doesn't skip the month in between.

/// Writes one `MetricsEntry` JSONL line per `(year_month, ts, model,
/// step_duration_ms, retry_count)` tuple, grouping same-month fixtures into a
/// single file the reader can enumerate.
fn write_metrics_entries(
    runtime: &OrbitRuntime,
    entries: &[(&str, DateTime<Utc>, &str, u64, u32)],
) {
    let mut by_month: std::collections::BTreeMap<&str, Vec<String>> =
        std::collections::BTreeMap::new();
    for (month, ts, model, step_duration_ms, retry_count) in entries {
        let line = json!({
            "ts": ts.to_rfc3339(),
            "job_run": "jrun-fixture",
            "step": "implement",
            "actor_identity": model,
            "tool_invocations": 1,
            "token_usage": 10,
            "step_duration_ms": step_duration_ms,
            "retry_count": retry_count,
        })
        .to_string();
        by_month.entry(month).or_default().push(line);
    }
    for (month, lines) in by_month {
        let dir = runtime
            .data_root()
            .join("state")
            .join("diagnostics")
            .join("metrics")
            .join(month);
        std::fs::create_dir_all(&dir).expect("create metrics month dir");
        write_lines(&dir.join("fixture.jsonl"), &lines);
    }
}

#[test]
fn months_in_range_single_month_when_since_and_now_share_a_month() {
    let since = DateTime::parse_from_rfc3339("2026-06-01T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let now = DateTime::parse_from_rfc3339("2026-06-15T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc);

    assert_eq!(months_in_range(since, now), vec!["2026-06".to_string()]);
}

/// Reproduces the original bug: 31 days before an early-March `now` lands in
/// January, so a naive "current month + prior month" guess never reads
/// February even though the window spans it.
#[test]
fn months_in_range_does_not_skip_february_on_early_march_dates() {
    let now = DateTime::parse_from_rfc3339("2026-03-01T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let since = now - Duration::days(30);
    assert_eq!(since.format("%Y-%m").to_string(), "2026-01");

    assert_eq!(
        months_in_range(since, now),
        vec![
            "2026-01".to_string(),
            "2026-02".to_string(),
            "2026-03".to_string(),
        ]
    );
}

#[test]
fn months_in_range_walks_a_leap_year_february() {
    let since = DateTime::parse_from_rfc3339("2028-01-30T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let now = DateTime::parse_from_rfc3339("2028-03-01T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc);

    assert_eq!(
        months_in_range(since, now),
        vec![
            "2028-01".to_string(),
            "2028-02".to_string(),
            "2028-03".to_string(),
        ]
    );
}

#[test]
fn months_in_range_spans_a_year_boundary() {
    let since = DateTime::parse_from_rfc3339("2025-12-20T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let now = DateTime::parse_from_rfc3339("2026-01-05T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc);

    assert_eq!(
        months_in_range(since, now),
        vec!["2025-12".to_string(), "2026-01".to_string()]
    );
}

/// Fixed timestamps (no wall-clock sleeps) with a record before, inside, and
/// after each windowed range prove only the eligible record is counted.
#[test]
fn compute_metrics_extras_excludes_records_outside_the_requested_window() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let now = DateTime::parse_from_rfc3339("2026-03-15T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let since = now - Duration::hours(1);
    let month = now.format("%Y-%m").to_string();

    write_metrics_entries(
        &runtime,
        &[
            // Before the window: excluded.
            (
                &month,
                since - Duration::minutes(1),
                "claude-opus-5",
                9_000,
                5,
            ),
            // Inside the window: the only eligible record.
            (
                &month,
                since + Duration::minutes(1),
                "claude-opus-5",
                400,
                2,
            ),
            // After "now" (future-dated / clock skew): excluded.
            (&month, now + Duration::minutes(10), "claude-opus-5", 500, 9),
        ],
    );

    let extras = compute_metrics_extras(&runtime, Some(since), now).expect("compute extras");
    let claude = extras.get("claude-opus-5").expect("claude-opus-5 row");
    assert_eq!(
        claude,
        &MetricsExtras {
            avg_duration_ms: 400,
            p95_duration_ms: 400,
            retry_count: 2,
        }
    );
}

#[test]
fn compute_metrics_extras_windows_1h_24h_7d_30d_each_include_only_the_inside_fixture() {
    let now = DateTime::parse_from_rfc3339("2026-03-15T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc);

    for (label, duration) in [
        ("1h", Duration::hours(1)),
        ("24h", Duration::hours(24)),
        ("7d", Duration::days(7)),
        ("30d", Duration::days(30)),
    ] {
        let runtime = OrbitRuntime::in_memory().expect("build runtime");
        let since = now - duration;
        let model = format!("claude-opus-5-{label}");
        write_metrics_entries(
            &runtime,
            &[
                (
                    &since.format("%Y-%m").to_string(),
                    since - Duration::minutes(1),
                    &model,
                    9_000,
                    5,
                ),
                (
                    &(since + (now - since) / 2).format("%Y-%m").to_string(),
                    since + (now - since) / 2,
                    &model,
                    300,
                    1,
                ),
                (
                    &now.format("%Y-%m").to_string(),
                    now + Duration::minutes(1),
                    &model,
                    500,
                    9,
                ),
            ],
        );

        let extras = compute_metrics_extras(&runtime, Some(since), now).expect("compute extras");
        let row = extras
            .get(model.as_str())
            .unwrap_or_else(|| panic!("{label}: expected a row for {model}"));
        assert_eq!(
            row.retry_count, 1,
            "{label}: only the inside-window record should count"
        );
        assert_eq!(
            row.avg_duration_ms, 300,
            "{label}: avg must ignore outside records"
        );
        assert_eq!(
            row.p95_duration_ms, 300,
            "{label}: p95 must ignore outside records"
        );
    }
}

/// Lifetime (`since == None`) must enumerate every metrics partition on disk,
/// not just the current and prior month, so records older than two months
/// are still counted.
#[test]
fn compute_metrics_extras_lifetime_includes_partitions_older_than_two_months() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let now = DateTime::parse_from_rfc3339("2026-03-15T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let old_ts = DateTime::parse_from_rfc3339("2025-11-01T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc);

    write_metrics_entries(&runtime, &[("2025-11", old_ts, "claude-opus-5", 200, 3)]);

    let extras = compute_metrics_extras(&runtime, None, now).expect("compute extras");
    let claude = extras.get("claude-opus-5").expect("claude-opus-5 row");
    assert_eq!(claude.retry_count, 3);
    assert_eq!(claude.avg_duration_ms, 200);
}

/// End-to-end: the HTTP handler must thread its parsed `?window=` and a
/// single request `now` into `compute_metrics_extras`, so a record just
/// outside a 1h window never reaches the response.
#[tokio::test]
async fn scoreboard_http_response_only_reflects_metrics_inside_the_requested_window() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let now = Utc::now();
    let month = now.format("%Y-%m").to_string();
    write_metrics_entries(
        &runtime,
        &[
            (
                &month,
                now - Duration::hours(2),
                "claude-opus-5-http",
                9_000,
                5,
            ),
            (
                &month,
                now - Duration::minutes(10),
                "claude-opus-5-http",
                400,
                2,
            ),
        ],
    );

    let response = get_scoreboard(runtime, Some("window=1h")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    let agent = &body["agents"]["claude-opus-5-http"];
    assert_eq!(agent["retries"].as_i64(), Some(2));
    assert_eq!(agent["avg_step_duration_ms"].as_i64(), Some(400));
    assert_eq!(agent["p95_wall_clock_ms"].as_i64(), Some(400));
}

// ORB-11201: a side-source (metrics extras / denials / failure incidents)
// read or query failure must surface as an explicit `unavailable` coverage
// note plus `null` per-agent fields, never as an indistinguishable measured
// zero. A source that succeeds — even with an empty result — still reports
// true zeros.

/// Forces a genuine, non-`InvalidInput` I/O error out of
/// `compute_metrics_extras`'s lifetime path (`list_metrics_months`'s
/// `fs::read_dir` over a plain file instead of a directory), proving the
/// error actually propagates instead of being silently treated as "no
/// partitions".
#[test]
fn compute_metrics_extras_surfaces_a_real_read_dir_failure() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let metrics_dir = runtime
        .data_root()
        .join("state")
        .join("diagnostics")
        .join("metrics");
    std::fs::create_dir_all(metrics_dir.parent().expect("diagnostics dir"))
        .expect("create diagnostics parent");
    std::fs::write(&metrics_dir, b"not a directory").expect("block the metrics directory");

    let result = compute_metrics_extras(&runtime, None, Utc::now());
    assert!(
        result.is_err(),
        "read_dir over a blocking file must be a real error, not an empty partition list"
    );
}

/// End-to-end: when the metrics log is unreadable, the scoreboard endpoint
/// still returns 200 with the rest of the summary intact, but marks the
/// metrics-derived fields `null` and the coverage note `unavailable` —
/// while the independent denials/failure-incidents sources (which have no
/// data to report) still come back as real, non-null zeros.
#[tokio::test]
async fn scoreboard_reports_metrics_extras_unavailable_not_zero_when_metrics_log_is_unreadable() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let metrics_dir = runtime
        .data_root()
        .join("state")
        .join("diagnostics")
        .join("metrics");
    std::fs::create_dir_all(metrics_dir.parent().expect("diagnostics dir"))
        .expect("create diagnostics parent");
    std::fs::write(&metrics_dir, b"not a directory").expect("block the metrics directory");

    let response = get_scoreboard(runtime, None).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "a side-source failure must not fail the whole scoreboard request"
    );
    let body = body_json(response).await;

    assert_eq!(
        body["coverage"]["metrics_extras"]["availability"].as_str(),
        Some("unavailable")
    );
    let claude = &body["agents"]["claude"];
    assert!(
        claude["avg_step_duration_ms"].is_null(),
        "a failed metrics join must not report a measured zero"
    );
    assert!(claude["retries"].is_null());
    assert!(claude["p95_wall_clock_ms"].is_null());

    // Independent sources succeeded (nothing to read) and report true zeros.
    assert_eq!(
        body["coverage"]["denials"]["availability"].as_str(),
        Some("observed")
    );
    assert_eq!(
        body["coverage"]["failure_incidents"]["availability"].as_str(),
        Some("observed")
    );
    assert_eq!(claude["denials"].as_i64(), Some(0));
    assert_eq!(claude["failure_incidents"].as_i64(), Some(0));
    assert_eq!(claude["unexpected_failure_incidents"].as_i64(), Some(0));
    assert_eq!(claude["failure_incident_events"].as_i64(), Some(0));
}

/// Directly exercises the merge policy: a failed source (`None`) must
/// produce `null` fields, while a succeeded-but-empty source (`Some` of an
/// empty map) must produce real zeros. Deterministic and independent of any
/// filesystem/SQLite failure injection.
#[test]
fn apply_side_source_extras_distinguishes_failed_source_from_true_zero() {
    let mut agents = serde_json::Map::new();
    agents.insert("claude".to_string(), json!({ "tasks_completed": 0 }));

    let metrics_extras: BTreeMap<String, MetricsExtras> = BTreeMap::new();
    let failure_rollup: BTreeMap<String, ActorFailureRollup> = BTreeMap::new();

    apply_side_source_extras(
        &mut agents,
        Some(&metrics_extras), // succeeded, empty -> true zero
        None,                  // failed -> null
        Some(&failure_rollup), // succeeded, empty -> true zero
    );

    let claude = &agents["claude"];
    assert_eq!(
        claude["avg_step_duration_ms"],
        json!(0),
        "a succeeded-but-empty metrics join is a true zero"
    );
    assert_eq!(claude["retries"], json!(0));
    assert_eq!(claude["p95_wall_clock_ms"], json!(0));
    assert!(
        claude["denials"].is_null(),
        "a failed denials join must not report a measured zero"
    );
    assert_eq!(claude["failure_incidents"], json!(0));
    assert_eq!(claude["unexpected_failure_incidents"], json!(0));
    assert_eq!(claude["failure_incident_events"], json!(0));
}

/// When every side source fails, the metrics-only "surface a new agent" path
/// must not fabricate a row: there is nothing observed to surface.
#[test]
fn apply_side_source_extras_adds_no_metrics_only_agent_when_metrics_source_failed() {
    let mut agents = serde_json::Map::new();

    apply_side_source_extras(&mut agents, None, None, None);

    assert!(
        agents.is_empty(),
        "a failed metrics source has no rows to surface as new agents"
    );
}
