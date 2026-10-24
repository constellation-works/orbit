//! `orbit.codebase.related` applies its own window default.
//!
//! An omitted `window` used to fall through `CodebaseQuery::parse` and answer
//! over 90 days, while the tool schema and the other surfaces say 30 days.
//! Undeclared `rollup` and regression-filter values are not related parameters.

use chrono::{DateTime, Utc};
use orbit_core::OrbitRuntime;
use serde_json::json;

use super::dispatch_admission::isolated;

#[test]
fn omitted_window_answers_over_thirty_days() {
    if !isolated("codebase_related::omitted_window_answers_over_thirty_days") {
        return;
    }
    let root =
        tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).expect("tempdir");
    let global = root.path().join("home/.orbit");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).expect("global orbit dir");
    std::fs::create_dir_all(&workspace).expect("workspace orbit dir");
    let runtime = OrbitRuntime::from_roots(&global, &workspace).expect("runtime");

    let related = runtime
        .run_tool(
            "orbit.codebase.related",
            json!({
                "path": "src/a.rs",
                "rollup": "not-a-rollup",
                "nature": "nope",
            }),
        )
        .expect("omitted window and undeclared rollup or nature are not invalid input");
    assert_eq!(
        related["window"], "30d",
        "an omitted window answered over 90d before the shared related default: {related}"
    );
    assert_eq!(related["rollup"], "file", "{related}");
    assert_eq!(related["top"], 10, "{related}");
    assert_eq!(related["min_support"], 15, "{related}");
    assert_window_age(&related["since"], 30);

    let wide = runtime
        .run_tool(
            "orbit.codebase.related",
            json!({"path": "src/a.rs", "window": "90d"}),
        )
        .expect("an explicit 90d window is still accepted");
    assert_eq!(wide["window"], "90d", "{wide}");
    assert_window_age(&wide["since"], 90);

    let analytics = runtime
        .run_tool("orbit.codebase.analytics", json!({}))
        .expect("analytics still accepts an empty selection");
    assert_eq!(
        analytics["window"], "90d",
        "analytics keeps the 90d default: {analytics}"
    );

    let refused = runtime
        .run_tool(
            "orbit.codebase.show",
            json!({"path": "src/a.rs", "nature": "nope"}),
        )
        .expect_err("show still rejects an unknown regression filter");
    assert!(
        refused.to_string().contains("nature"),
        "show must still parse nature: {refused}"
    );
}

fn assert_window_age(since: &serde_json::Value, days: i64) {
    let since = since
        .as_str()
        .unwrap_or_else(|| panic!("since for {days}d: {since}"));
    let since = DateTime::parse_from_rfc3339(since).unwrap_or_else(|error| {
        panic!("since {since} is not RFC3339: {error}");
    });
    let age_hours = Utc::now()
        .signed_duration_since(since.with_timezone(&Utc))
        .num_hours();
    assert!(
        (age_hours - days * 24).abs() <= 2,
        "window of {days}d has since {since} ({age_hours} hours ago)"
    );
}
