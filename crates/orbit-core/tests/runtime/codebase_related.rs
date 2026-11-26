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
fn file_rollup_marks_missing_and_replaced_paths_absent_in_analytics_and_show() {
    if !isolated(
        "codebase_related::file_rollup_marks_missing_and_replaced_paths_absent_in_analytics_and_show",
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
    std::fs::create_dir_all(repo.join("src/nested")).expect("nested directory");
    std::fs::create_dir_all(&workspace).expect("workspace");
    std::fs::write(repo.join("src/live.rs"), "live").expect("file");
    std::fs::write(repo.join("src/replaced.rs"), "historical file").expect("historical file");
    std::fs::write(repo.join("src/nested/child.rs"), "historical nested file")
        .expect("historical nested file");
    let runtime = OrbitRuntime::from_roots(&global, &workspace).expect("runtime");
    let add = |title: &str, status, relations| {
        runtime
            .add_task(TaskAddParams {
                title: title.into(),
                status: Some(status),
                context_files: vec![
                    "file:src/live.rs".into(),
                    "file:src/gone.rs".into(),
                    "file:src/replaced.rs".into(),
                    "file:src/nested/child.rs".into(),
                ],
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
                sites: vec![
                    "file:src/live.rs".into(),
                    "file:src/gone.rs".into(),
                    "file:src/replaced.rs".into(),
                    "file:src/nested/child.rs".into(),
                ],
            }),
        }],
    );
    let coupled = runtime
        .add_task(TaskAddParams {
            title: "coupled".into(),
            status: Some(TaskStatus::Backlog),
            context_files: vec!["file:src/live.rs".into(), "file:src/nested/child.rs".into()],
            ..Default::default()
        })
        .expect("coupled task");
    let reserved = orbit_core::runtime::stored::locks::reserve(
        &runtime,
        json!({"task_ids": [coupled.id.to_string()], "ttl_seconds": 600}),
        None,
        None,
        None,
    )
    .expect("reserve coupled task");
    assert_eq!(reserved["reserved"], true, "{reserved}");
    let query = |rollup: &str| {
        runtime
            .run_tool(
                "orbit.codebase.analytics",
                json!({"window":"all", "min_support":1, "rollup":rollup}),
            )
            .expect("analytics")
    };
    let before_replacement = query("file");
    let previous_replaced = before_replacement["regressions"]["rows"]
        .as_array()
        .expect("previous rows")
        .iter()
        .find(|row| row["path"] == "src/replaced.rs")
        .expect("historical file row before replacement");
    assert!(previous_replaced.get("absent_at_head").is_none());
    let previous_nested = before_replacement["regressions"]["rows"]
        .as_array()
        .expect("previous rows")
        .iter()
        .find(|row| row["path"] == "src/nested/child.rs")
        .expect("historical nested row before replacement");
    assert!(previous_nested.get("absent_at_head").is_none());

    std::fs::remove_file(repo.join("src/replaced.rs")).expect("remove historical file");
    std::fs::create_dir(repo.join("src/replaced.rs")).expect("replace file with directory");
    std::fs::remove_file(repo.join("src/nested/child.rs")).expect("remove nested file");
    std::fs::remove_dir(repo.join("src/nested")).expect("remove nested directory");
    std::fs::write(repo.join("src/nested"), "replaced by file")
        .expect("replace directory with file");
    let analytics = query("file");
    let rows = analytics["regressions"]["rows"].as_array().expect("rows");
    let gone = rows
        .iter()
        .find(|row| row["path"] == "src/gone.rs")
        .expect("missing row");
    let replaced = rows
        .iter()
        .find(|row| row["path"] == "src/replaced.rs")
        .expect("historical file row");
    let live = rows
        .iter()
        .find(|row| row["path"] == "src/live.rs")
        .expect("existing row");
    assert_eq!(gone["absent_at_head"], true);
    assert_eq!(replaced["absent_at_head"], true);
    assert!(live.get("absent_at_head").is_none());
    assert!(
        replaced["numerator"]
            .as_u64()
            .is_some_and(|count| count > 0)
    );
    assert!(
        replaced["denominator"]
            .as_u64()
            .is_some_and(|count| count > 0)
    );
    assert!(
        !replaced["regressed_tasks"]
            .as_array()
            .expect("history")
            .is_empty()
    );
    assert_eq!(replaced["numerator"], previous_replaced["numerator"]);
    assert_eq!(replaced["denominator"], previous_replaced["denominator"]);
    assert_eq!(
        replaced["regressed_tasks"],
        previous_replaced["regressed_tasks"]
    );
    assert_eq!(replaced["sites"], previous_replaced["sites"]);
    let nested = rows
        .iter()
        .find(|row| row["path"] == "src/nested/child.rs")
        .expect("nested file row");
    assert_eq!(nested["absent_at_head"], true, "{nested}");
    assert!(nested["numerator"].as_u64().is_some_and(|count| count > 0));
    assert_eq!(nested["numerator"], previous_nested["numerator"]);
    assert_eq!(nested["denominator"], previous_nested["denominator"]);
    assert_eq!(
        nested["regressed_tasks"],
        previous_nested["regressed_tasks"]
    );
    let pair = analytics["change_coupling"]["rows"]
        .as_array()
        .expect("coupling rows")
        .iter()
        .find(|row| {
            row["paths"]
                .as_array()
                .is_some_and(|paths| paths.iter().any(|path| path == "src/nested/child.rs"))
        })
        .expect("coupling pair naming the nested file");
    assert_eq!(
        pair["absent_paths_at_head"],
        json!(["src/nested/child.rs"]),
        "{pair}"
    );
    let detail = runtime
        .run_tool(
            "orbit.codebase.show",
            json!({"path":"src/gone.rs", "window":"all", "min_support":1}),
        )
        .expect("detail");
    assert_eq!(detail["absent_at_head"], true);
    let replaced_detail = runtime
        .run_tool(
            "orbit.codebase.show",
            json!({"path":"src/replaced.rs", "window":"all", "min_support":1}),
        )
        .expect("replaced file detail");
    assert_eq!(replaced_detail["absent_at_head"], true);
    let nested_detail = runtime
        .run_tool(
            "orbit.codebase.show",
            json!({"path":"src/nested/child.rs", "window":"all", "min_support":1}),
        )
        .expect("nested file detail");
    assert_eq!(nested_detail["absent_at_head"], true, "{nested_detail}");
    let detail_regressions = &replaced_detail["rankings"]["regressions"];
    assert_eq!(detail_regressions["numerator"], replaced["numerator"]);
    assert_eq!(detail_regressions["denominator"], replaced["denominator"]);
    assert!(
        !detail_regressions["tasks"]
            .as_array()
            .expect("history")
            .is_empty()
    );
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
