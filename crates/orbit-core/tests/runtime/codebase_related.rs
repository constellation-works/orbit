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

#[test]
fn analytics_marks_missing_checkout_paths_without_changing_rollups() {
    if !isolated(
        "codebase_related::analytics_marks_missing_checkout_paths_without_changing_rollups",
    ) {
        return;
    }
    use orbit_core::application::task::TaskAddParams;
    use orbit_types::task::{
        DefectSeverity, RegressionClassification, RegressionEscape, RegressionNature,
        SeveritySource, TaskRelation, TaskRelationType, TaskStatus,
    };
    let root =
        tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).expect("tempdir");
    let global = root.path().join("home/.orbit");
    let repo = root.path().join("repo");
    let workspace = repo.join(".orbit");
    std::fs::create_dir_all(&global).expect("global");
    std::fs::create_dir_all(repo.join("src")).expect("src");
    std::fs::create_dir_all(&workspace).expect("workspace");
    std::fs::write(repo.join("src/live.rs"), "live").expect("file");
    let runtime = OrbitRuntime::from_roots(&global, &workspace).expect("runtime");
    let add = |title: &str, status, relations| {
        runtime
            .add_task(TaskAddParams {
                title: title.into(),
                status: Some(status),
                context_files: vec!["file:src/live.rs".into(), "file:src/gone.rs".into()],
                relations,
                ..Default::default()
            })
            .expect("task")
    };
    let base = add("base", TaskStatus::Done, vec![]);
    add(
        "fix",
        TaskStatus::Backlog,
        vec![TaskRelation {
            relation_type: TaskRelationType::RegressionFrom,
            target: base.id,
            regression: Some(RegressionClassification {
                nature: RegressionNature::Unclassified,
                escape: RegressionEscape::Unclassified,
                severity: DefectSeverity::Medium,
                severity_source: SeveritySource::Filer,
                sites: vec!["file:src/live.rs".into(), "file:src/gone.rs".into()],
            }),
        }],
    );
    let query = |rollup: &str| {
        runtime
            .run_tool(
                "orbit.codebase.analytics",
                json!({"window":"all", "min_support":1, "rollup":rollup}),
            )
            .expect("analytics")
    };
    let analytics = query("file");
    let rows = analytics["regressions"]["rows"].as_array().expect("rows");
    let gone = rows
        .iter()
        .find(|row| row["path"] == "src/gone.rs")
        .expect("missing row");
    let live = rows
        .iter()
        .find(|row| row["path"] == "src/live.rs")
        .expect("existing row");
    assert_eq!(gone["absent_at_head"], true);
    assert!(live.get("absent_at_head").is_none());
    let detail = runtime
        .run_tool(
            "orbit.codebase.show",
            json!({"path":"src/gone.rs", "window":"all", "min_support":1}),
        )
        .expect("detail");
    assert_eq!(detail["absent_at_head"], true);
    for rollup in ["dir", "crate"] {
        let result = query(rollup);
        assert!(
            result["regressions"]["rows"]
                .as_array()
                .expect("rows")
                .iter()
                .all(|row| row.get("absent_at_head").is_none())
        );
    }
}
