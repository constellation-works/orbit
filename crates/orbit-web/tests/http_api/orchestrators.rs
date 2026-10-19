//! The orchestrator scorecard (`/api/orchestrators/*`) through the HTTP API,
//! over fixture tasks owned by two orchestrators.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use orbit_core::TaskStatus;
use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
use orbit_types::task::{
    ACCEPTANCE_CRITERIA_DOCUMENT, DESCRIPTION_DOCUMENT, ExternalRef, GITHUB_PR_EXTERNAL_REF_SYSTEM,
    RegressionClassification, RegressionEscape, RegressionNature, TASK_ARTIFACT_SCHEMA_VERSION,
    TASK_EVENTS_FILE_NAME, TaskComplexity, TaskEventRowV2, TaskOrigin, TaskRelation,
    TaskRelationType, task_fields_updated_note,
};
use orbit_types::workflow::{
    AgentBlocker, TASK_BLOCKED_BY_AGENT_EVENT, task_blocked_by_agent_message,
};
use serde_json::{Value, json};

use super::support::{Fixture, isolated, json_ok};

const WS: &str = "workspace=ws_http_fixture";

struct Owned<'a> {
    title: &'a str,
    orchestrator: Option<&'a str>,
    origin: Option<TaskOrigin>,
    complexity: TaskComplexity,
    status: TaskStatus,
}

fn add(fixture: &Fixture, task: Owned<'_>) -> String {
    add_full(fixture, task, vec![], vec![])
}

fn add_full(
    fixture: &Fixture,
    task: Owned<'_>,
    relations: Vec<TaskRelation>,
    external_refs: Vec<ExternalRef>,
) -> String {
    let id = fixture
        .runtime
        .add_task(TaskAddParams {
            title: task.title.into(),
            description: "orchestrator scorecard fixture".into(),
            orchestrator: task.orchestrator.map(str::to_string),
            origin: task.origin,
            complexity: task.complexity,
            status: Some(TaskStatus::Backlog),
            relations,
            external_refs,
            ..Default::default()
        })
        .unwrap()
        .id
        .to_string();
    // An orchestrator is attributed while a task is in the backlog; settle it
    // after, as the human override does.
    if task.status != TaskStatus::Backlog {
        fixture
            .runtime
            .force_update_task_with_identity(
                &id,
                TaskUpdateParams {
                    status: Some(task.status),
                    ..Default::default()
                },
                None,
                None,
            )
            .unwrap();
    }
    id
}

fn bundle_dir(root: &Path, task_id: &str) -> Option<PathBuf> {
    for entry in fs::read_dir(root).ok()? {
        let path = entry.ok()?.path();
        if !path.is_dir() {
            continue;
        }
        if path.file_name().is_some_and(|name| name == task_id)
            && path.join(TASK_EVENTS_FILE_NAME).is_file()
        {
            return Some(path);
        }
        if let Some(found) = bundle_dir(&path, task_id) {
            return Some(found);
        }
    }
    None
}

/// Append one history row to a disposable fixture task, as a lifecycle writer
/// would: runs, blocks and requeues have no public operator entry point that
/// a fixture can drive without a crew.
fn append(
    fixture: &Fixture,
    task_id: &str,
    at: DateTime<Utc>,
    event: &str,
    note: Option<String>,
    to_status: Option<TaskStatus>,
) {
    let dir = bundle_dir(fixture.path("").as_path(), task_id).expect("fixture task bundle");
    let path = dir.join(TASK_EVENTS_FILE_NAME);
    let rows = fs::read_to_string(&path).unwrap().lines().count();
    let row = TaskEventRowV2 {
        schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
        event_id: format!("EV-{:04}", rows + 1),
        at,
        by: "system".into(),
        event_type: event.into(),
        note,
        from_status: None,
        to_status,
        operation_id: None,
        decision: None,
    };
    let mut file = fs::OpenOptions::new().append(true).open(path).unwrap();
    writeln!(file, "{}", serde_json::to_string(&row).unwrap()).unwrap();
}

/// Edit a task's specification or comment on it through the task update
/// path, as an operator or orchestrator session does.
fn update(fixture: &Fixture, task_id: &str, params: TaskUpdateParams) {
    fixture
        .runtime
        .update_task_as_human(task_id, params, "fixture".into())
        .unwrap();
}

fn row<'a>(payload: &'a Value, orchestrator: &str) -> &'a Value {
    payload["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["orchestrator"] == orchestrator)
        .unwrap_or_else(|| panic!("no {orchestrator} row: {payload}"))
}

fn component<'a>(detail: &'a Value, key: &str) -> &'a Value {
    detail["components"]
        .as_array()
        .unwrap()
        .iter()
        .find(|component| component["key"] == key)
        .unwrap_or_else(|| panic!("no {key} component: {detail}"))
}

fn component_tasks(detail: &Value, key: &str) -> Vec<String> {
    let mut tasks: Vec<String> = component(detail, key)["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|task| task["task_id"].as_str().unwrap().to_string())
        .collect();
    tasks.sort();
    tasks
}

fn sorted(ids: &[&str]) -> Vec<String> {
    let mut ids: Vec<String> = ids.iter().map(|id| id.to_string()).collect();
    ids.sort();
    ids
}

fn operations(trace: &[Value], name: &str) -> usize {
    trace
        .iter()
        .filter(|event| event["span"]["name"] == name)
        .count()
}

/// Two orchestrators over one fixture workspace: the scorecard reports each
/// one's throughput, proactivity, spec defects and rework from history,
/// relations and comments, normalized per 100 owned tasks and per 8 active
/// hours with support counts; every count drills down to its tasks; and the
/// tool serves the same rows from one cached scan.
#[test]
fn orchestrator_scorecard_counts_components_from_history_and_drills_down() {
    isolated(
        "orchestrators::orchestrator_scorecard_counts_components_from_history_and_drills_down",
        || {
            let fixture = Fixture::new();
            let now = Utc::now();

            // opus: a landed hard initiative task that was blocked on its
            // criteria, requeued by hand, re-specified and run again.
            let landed = add(
                &fixture,
                Owned {
                    title: "landed initiative",
                    orchestrator: Some("opus"),
                    origin: Some(TaskOrigin::OrchestratorInitiative),
                    complexity: TaskComplexity::Hard,
                    status: TaskStatus::Done,
                },
            );
            append(
                &fixture,
                &landed,
                now - Duration::hours(3),
                "started",
                None,
                Some(TaskStatus::InProgress),
            );
            append(
                &fixture,
                &landed,
                now - Duration::hours(2),
                TASK_BLOCKED_BY_AGENT_EVENT,
                Some(task_blocked_by_agent_message(&AgentBlocker {
                    kind: "acceptance_criteria_unmet".into(),
                    evidence: "criterion 2 contradicts criterion 3".into(),
                })),
                Some(TaskStatus::Blocked),
            );
            append(
                &fixture,
                &landed,
                now - Duration::minutes(90),
                "operator_requeued",
                None,
                Some(TaskStatus::Backlog),
            );
            append(
                &fixture,
                &landed,
                now - Duration::minutes(80),
                "updated",
                Some(task_fields_updated_note(&[
                    DESCRIPTION_DOCUMENT,
                    ACCEPTANCE_CRITERIA_DOCUMENT,
                ])),
                None,
            );
            append(
                &fixture,
                &landed,
                now - Duration::minutes(60),
                "started",
                None,
                Some(TaskStatus::InProgress),
            );
            append(
                &fixture,
                &landed,
                now - Duration::minutes(30),
                "status_changed",
                Some("delivered by pull request #12 merged as abc1234".into()),
                Some(TaskStatus::Done),
            );

            // opus: an operator request whose description was edited
            // before its first run started, which is not a spec defect.
            let edited_before = add(
                &fixture,
                Owned {
                    title: "edited before its run",
                    orchestrator: Some("opus"),
                    origin: Some(TaskOrigin::OperatorRequest),
                    complexity: TaskComplexity::Medium,
                    status: TaskStatus::Backlog,
                },
            );
            update(
                &fixture,
                &edited_before,
                TaskUpdateParams {
                    description: Some("clarified before any run".into()),
                    ..Default::default()
                },
            );
            append(
                &fixture,
                &edited_before,
                Utc::now(),
                "started",
                None,
                Some(TaskStatus::InProgress),
            );
            append(
                &fixture,
                &edited_before,
                Utc::now(),
                "status_changed",
                None,
                Some(TaskStatus::Backlog),
            );

            // opus: no typed origin, criteria edited after its run started,
            // and a review gate rejection.
            let edited_after = add(
                &fixture,
                Owned {
                    title: "edited after its run",
                    orchestrator: Some("opus"),
                    origin: None,
                    complexity: TaskComplexity::Low,
                    status: TaskStatus::Backlog,
                },
            );
            append(
                &fixture,
                &edited_after,
                now - Duration::minutes(50),
                "started",
                None,
                Some(TaskStatus::InProgress),
            );
            append(
                &fixture,
                &edited_after,
                now - Duration::minutes(40),
                "status_changed",
                None,
                Some(TaskStatus::Backlog),
            );
            update(
                &fixture,
                &edited_after,
                TaskUpdateParams {
                    acceptance_criteria: Some(vec!["a criterion added mid-flight".into()]),
                    ..Default::default()
                },
            );
            update(
                &fixture,
                &edited_after,
                TaskUpdateParams {
                    comment: Some(
                        "before-PR review settled attempt `rv-1`: verdict **reject** (2 findings)"
                            .into(),
                    ),
                    ..Default::default()
                },
            );

            // sonnet: a rejected initiative task whose PR never merged.
            let dropped = add_full(
                &fixture,
                Owned {
                    title: "dropped initiative",
                    orchestrator: Some("sonnet"),
                    origin: Some(TaskOrigin::OrchestratorInitiative),
                    complexity: TaskComplexity::Medium,
                    status: TaskStatus::Rejected,
                },
                vec![],
                vec![ExternalRef {
                    system: GITHUB_PR_EXTERNAL_REF_SYSTEM.into(),
                    id: "7".into(),
                    url: None,
                }],
            );

            // Unattributed tasks, one of them an incomplete-spec regression
            // of the landed task.
            add(
                &fixture,
                Owned {
                    title: "unattributed",
                    orchestrator: None,
                    origin: None,
                    complexity: TaskComplexity::Low,
                    status: TaskStatus::Backlog,
                },
            );
            add_full(
                &fixture,
                Owned {
                    title: "regression",
                    orchestrator: None,
                    origin: None,
                    complexity: TaskComplexity::Low,
                    status: TaskStatus::Backlog,
                },
                vec![TaskRelation {
                    relation_type: TaskRelationType::RegressionFrom,
                    target: landed.parse().unwrap(),
                    regression: Some(RegressionClassification {
                        nature: RegressionNature::Incomplete,
                        escape: RegressionEscape::NoTest,
                    }),
                }],
                vec![],
            );

            let server = fixture.counted_task_server();
            let before = server.task_query_trace().len();
            let scorecard = json_ok(server.get(&format!(
                "/api/orchestrators/scorecard?{WS}&window=30d&min_support=3"
            )));
            let cold = server.task_query_trace()[before..].to_vec();
            assert_eq!(
                operations(&cold, "task_history_read"),
                4,
                "the scan reads each attributed task's history once: {cold:?}"
            );
            assert_eq!(scorecard["attributed_tasks"], 4, "{scorecard}");
            assert_eq!(scorecard["unattributed_tasks"], 2, "{scorecard}");
            assert_eq!(
                scorecard["rows"].as_array().map(Vec::len),
                Some(2),
                "one row per orchestrator: {scorecard}"
            );

            let opus = row(&scorecard, "opus");
            let counts = &opus["counts"];
            let expected = [
                ("owned", 3),
                ("landed", 1),
                ("weighted_landed", 4),
                ("landed_initiative", 1),
                ("dropped_initiative", 0),
                ("net_initiative", 1),
                ("unknown_origin", 1),
                ("spec_edits", 2),
                ("criteria_blocks", 1),
                ("criteria_escalations", 0),
                ("spec_regressions", 1),
                ("spec_defects", 4),
                ("review_rejects", 1),
                ("repeat_runs", 1),
                ("operator_requeues", 1),
                ("pr_rework", 0),
                ("rework", 3),
            ];
            for (key, count) in expected {
                assert_eq!(counts[key], count, "opus {key}: {opus}");
            }
            assert_eq!(opus["below_support"], false);
            assert_eq!(
                opus["origins"],
                json!({"operator_request": 1, "orchestrator_initiative": 1, "automation": 0, "unknown": 1})
            );
            assert_eq!(
                opus["rework_by_cause"],
                json!({"spec": 2, "crew": 1, "infrastructure": 0, "other": 0}),
                "the requeue and rerun followed a criteria block; the review reject is the crew's"
            );
            assert_eq!(opus["support"]["owned_tasks"], 3);
            assert_eq!(opus["support"]["tasks_with_runs"], 3);
            // A task is active from its first run start to its last run end:
            // 3h→30m ago spans the other tasks' runs, so 2.5 active hours.
            assert_eq!(opus["support"]["active_hours"], 2.5, "{opus}");
            assert_eq!(opus["per_100_tasks"]["spec_edits"], 66.67);
            assert_eq!(opus["per_8_hours"]["spec_edits"], 6.4);
            assert_eq!(opus["landed_share"], 1.0);
            // 4 weighted landed + 1 landed initiative − 4 spec defects − 3 rework.
            assert_eq!(opus["composite"]["value"], -2.0);
            assert_eq!(opus["composite"]["per_100_tasks"], -66.67);

            let sonnet = row(&scorecard, "sonnet");
            assert_eq!(sonnet["below_support"], true, "one owned task is below 3");
            assert_eq!(sonnet["counts"]["dropped_initiative"], 1);
            assert_eq!(sonnet["counts"]["net_initiative"], -1);
            assert_eq!(
                sonnet["counts"]["pr_rework"], 1,
                "the unmerged PR is rework"
            );
            assert_eq!(sonnet["landed_share"], 0.0);
            assert_eq!(
                sonnet["per_8_hours"]["owned"],
                Value::Null,
                "no run means no active time to normalize by"
            );
            assert_eq!(
                scorecard["rows"][0]["orchestrator"], "opus",
                "rows at minimum support lead"
            );
            let weights = &scorecard["definitions"]["composite"]["weights"];
            assert_eq!(weights.as_array().map(Vec::len), Some(4), "{weights}");

            // Each count drills down to its tasks, from the cached scan.
            let before = server.task_query_trace().len();
            let detail = json_ok(server.get(&format!(
                "/api/orchestrators/detail?{WS}&window=30d&min_support=3&orchestrator=opus"
            )));
            let warm = server.task_query_trace()[before..].to_vec();
            assert_eq!(
                operations(&warm, "task_history_read"),
                0,
                "a drill-down inside the cache period reuses the scan: {warm:?}"
            );
            assert_eq!(detail["row"], *opus);
            assert_eq!(
                component_tasks(&detail, "spec_edits"),
                sorted(&[&landed, &edited_after]),
                "an edit before the first run start is not a spec defect"
            );
            assert_eq!(
                component_tasks(&detail, "criteria_blocks"),
                sorted(&[&landed])
            );
            assert_eq!(
                component_tasks(&detail, "spec_regressions"),
                sorted(&[&landed])
            );
            assert_eq!(
                component_tasks(&detail, "review_rejects"),
                sorted(&[&edited_after])
            );
            assert_eq!(component_tasks(&detail, "repeat_runs"), sorted(&[&landed]));
            assert_eq!(
                component_tasks(&detail, "operator_requeues"),
                sorted(&[&landed])
            );
            assert_eq!(
                component_tasks(&detail, "owned"),
                sorted(&[&landed, &edited_before, &edited_after])
            );
            let rerun = &component(&detail, "repeat_runs")["tasks"][0]["occurrences"][0];
            assert_eq!(rerun["cause"], "spec", "{rerun}");
            for component in detail["components"].as_array().unwrap() {
                let total: i64 = component["tasks"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|task| task["count"].as_i64().unwrap())
                    .sum();
                assert_eq!(
                    component["count"],
                    opus["counts"][component["key"].as_str().unwrap()],
                    "a drill-down's tasks sum to the row's count: {component}"
                );
                assert_eq!(component["count"], total, "{component}");
            }
            let helper_detail = json_ok(server.get(&format!(
                "/api/orchestrators/detail?{WS}&min_support=3&orchestrator=sonnet"
            )));
            assert_eq!(
                component_tasks(&helper_detail, "dropped_initiative"),
                sorted(&[&dropped])
            );

            // The tool serves the route's rows.
            let tool = fixture
                .runtime
                .run_tool(
                    "orbit.orchestrator.scorecard",
                    json!({"window": "30d", "min_support": 3}),
                )
                .unwrap();
            assert_eq!(tool["rows"], scorecard["rows"]);

            for refused in [
                format!("/api/orchestrators/scorecard?{WS}&window=1y"),
                format!("/api/orchestrators/scorecard?{WS}&min_support=0"),
                format!("/api/orchestrators/detail?{WS}"),
                format!("/api/orchestrators/detail?{WS}&orchestrator=nobody"),
            ] {
                assert_eq!(server.get(&refused).status().as_u16(), 400, "{refused}");
            }
        },
    );
}
