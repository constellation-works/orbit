//! Every auto-task listing reports one decision: the CLI table and records,
//! MCP `orbit.auto_task.list` and its bounded view agree on whether a
//! definition fires here and why not.

use std::fs;

use chrono::{Duration, Utc};
use orbit_core::AutoTaskAddParams;
use orbit_types::task::{TaskPriority, TaskStatus, TaskType};
use orbit_types::workflow::{AutoTaskSchedule, AutoTaskTemplate, DedupePolicy};
use serde_json::{Value, json};

use crate::auto_task_lifecycle_cli::git;
use crate::isolated_cli_fixture::Fixture;
use crate::review_after_landing_cli::{
    commit, in_isolated_child, open_runtime, retarget, toggle, trigger,
};

const CONSUMER: &str = "delivery-code-review";

fn named<'a>(rows: &'a Value, name: &str) -> &'a Value {
    rows.as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == name)
        .unwrap_or_else(|| panic!("{name} is listed: {rows}"))
}

/// The bounded view is an operator surface.
fn bounded(fixture: &Fixture) -> Value {
    let input = json!({"view":"bounded","workspace":"audit-qa","limit":50}).to_string();
    let output = fixture
        .command(&["tool", "run", "orbit.auto_task.list", "--input", &input])
        .env("ORBIT_OPERATOR", "1")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    serde_json::from_slice(&output).unwrap()
}

/// [ORB-12867] An enabled delivery definition another machine owns never
/// fires here. The CLI already said so; MCP and its bounded view now report
/// the same reason and a non-live state instead of looking live.
#[test]
fn every_listing_reports_a_delivery_definition_owned_elsewhere_as_skipped() {
    const TEST: &str = "auto_task_listing_cli::every_listing_reports_a_delivery_definition_owned_elsewhere_as_skipped";
    if !in_isolated_child(TEST) {
        return;
    }

    let fixture = Fixture::new();
    git(&fixture, &["checkout", "-b", "fixture-delivery"]);
    commit(&fixture, "baseline\n");
    let mut elsewhere = trigger();
    elsewhere["owner_machine"] = json!("hm_elsewhere");
    retarget(&fixture, &elsewhere);
    toggle(&fixture, "on");

    let records = fixture.json(&["auto-task", "list", "--format", "json"]);
    let record = named(&records, CONSUMER);
    let reason = record["skipped_reason"]
        .as_str()
        .unwrap_or_else(|| panic!("an unadmittable definition names why: {record}"));
    assert!(reason.contains("hm_elsewhere"), "{reason}");
    assert_eq!(record["plugin_inactive"], false, "{record}");
    assert_eq!(record["effective_enabled"], true, "{record}");

    let table = fixture
        .command(&["auto-task", "list", "--format", "plain"])
        .assert()
        .success();
    let output = table.get_output();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let row = stdout
        .lines()
        .find(|line| line.starts_with(CONSUMER))
        .unwrap_or_else(|| panic!("{stdout}"));
    assert_eq!(row.split('\t').nth(1), Some("skipped"), "{row}");
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains(&format!("skipped [{CONSUMER}]: {reason}")),
        "the skip notice keeps its wording on stderr: {output:?}"
    );

    let runtime = open_runtime(&fixture);
    let plain = runtime
        .run_tool_as_human("orbit.auto_task.list", json!({}))
        .unwrap();
    let listed = named(&plain, CONSUMER);
    assert_eq!(listed["skipped_reason"], reason, "{listed}");
    assert_eq!(listed["effective_enabled"], true, "{listed}");
    assert!(listed.get("plugin_inactive").is_none(), "{listed}");

    let page = bounded(&fixture);
    let item = named(&page["items"], CONSUMER);
    assert_eq!(item["state"], "unavailable", "{item}");
    assert_eq!(item["skip_reason"], reason, "{item}");
    assert!(item["next_due"].is_null(), "{item}");
}

/// The bounded view degrades per row as the dashboard does: an unreadable
/// host cursor or a schedule that cannot be projected marks only the rows it
/// affects unavailable, instead of failing the whole read.
#[test]
fn bounded_listing_marks_unprojectable_rows_unavailable_without_failing() {
    const TEST: &str = "auto_task_listing_cli::bounded_listing_marks_unprojectable_rows_unavailable_without_failing";
    if !in_isolated_child(TEST) {
        return;
    }

    let fixture = Fixture::new();
    let runtime = open_runtime(&fixture);
    for (name, schedule) in [
        ("hourly", AutoTaskSchedule::Interval { every_minutes: 60 }),
        (
            // Parses, yet no 30 February ever arrives.
            "never",
            AutoTaskSchedule::Cron {
                cron: "0 0 30 2 *".into(),
            },
        ),
    ] {
        runtime
            .auto_task_add(AutoTaskAddParams {
                name: name.into(),
                description: "listing fixture".into(),
                schedule,
                template: AutoTaskTemplate {
                    title: format!("Listing {name}"),
                    description: "Bounded listing fixture".into(),
                    acceptance_criteria: vec![],
                    task_type: TaskType::Chore,
                    tags: vec![],
                    required_tools: vec![],
                    context_files: vec![],
                    priority: TaskPriority::Medium,
                    complexity: None,
                    crew: None,
                    status: TaskStatus::Backlog,
                },
                dedupe: DedupePolicy::SkipIfOpen,
            })
            .unwrap();
    }
    let cursor = orbit_core::application::auto_tasks::cursor_state_path(&runtime.paths().state_dir);
    let baseline = (Utc::now() - Duration::minutes(10)).to_rfc3339();
    fs::create_dir_all(cursor.parent().unwrap()).unwrap();
    fs::write(
        &cursor,
        json!({"definitions":{
            "hourly":{"baseline_at":baseline},
            "never":{"baseline_at":baseline},
        }})
        .to_string(),
    )
    .unwrap();

    let page = bounded(&fixture);
    assert!(page["cursor_state_error"].is_null(), "{page}");
    let hourly = named(&page["items"], "hourly");
    assert_eq!(hourly["state"], "scheduled", "{hourly}");
    assert!(hourly["next_due"].is_string(), "{hourly}");
    let never = named(&page["items"], "never");
    assert_eq!(never["state"], "unavailable", "{never}");
    assert!(never["next_due"].is_null(), "{never}");

    fs::write(&cursor, "{broken").unwrap();
    let page = bounded(&fixture);
    assert!(page["cursor_state_error"].is_string(), "{page}");
    let hourly = named(&page["items"], "hourly");
    assert_eq!(
        hourly["state"], "unavailable",
        "a corrupt cursor is not silently never observed: {hourly}"
    );
    assert!(hourly["next_due"].is_null(), "{hourly}");
    assert_eq!(fs::read_to_string(&cursor).unwrap(), "{broken");
}
