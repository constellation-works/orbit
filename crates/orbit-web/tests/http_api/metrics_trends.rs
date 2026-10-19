//! Analytics › Metrics trends [ORB-15746]: implement_one durations, terminal
//! task transitions and filed defects, bucketed on the clock and scoped to the
//! workspace.

use chrono::{DateTime, Duration, Utc};
use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
use orbit_core::{JobRunState, JobRunStep, JobTargetType};
use orbit_store::contracts::InvocationInsertParams;
use orbit_types::task::{TaskComplexity, TaskRelation, TaskRelationType, TaskStatus, TaskType};
use orbit_types::telemetry::InvocationTrace;
use orbit_types::workflow::PipelineState;
use serde_json::{Value, json};

use super::support::{Fixture, isolated, json_ok};

const WS: &str = "workspace=ws_http_fixture";
const MINUTE_MS: u64 = 60_000;

fn add_task(fixture: &Fixture, complexity: TaskComplexity, status: TaskStatus) -> String {
    fixture
        .runtime
        .add_task(TaskAddParams {
            title: format!("{complexity:?} {status:?} metrics fixture"),
            description: "metrics trend fixture".into(),
            complexity,
            status: Some(status),
            ..Default::default()
        })
        .unwrap()
        .id
        .to_string()
}

/// One finished implement_one run, then moved to `at` (and, for `partition`,
/// to another workspace's rows) the way only the store can.
fn record_run(
    fixture: &Fixture,
    run: &str,
    task: Option<&str>,
    minutes: u64,
    at: DateTime<Utc>,
    partition: Option<&str>,
) {
    outcome(fixture, run, at, JobRunState::Success, "performed");
    fixture
        .runtime
        .insert_invocation_trace_record(&InvocationInsertParams {
            job_run_id: run.into(),
            activity_id: "implement_one".into(),
            agent: "codex".into(),
            provider: None,
            model: Some("gpt-6.1-sol".into()),
            task_ids: task.map(str::to_string).into_iter().collect(),
            trace: InvocationTrace {
                duration_ms: minutes * MINUTE_MS,
                ..Default::default()
            },
        })
        .unwrap();
    fixture
        .runtime
        .sqlite_store()
        .unwrap()
        .with_transaction(|tx| {
            tx.connection()
                .execute(
                    "UPDATE invocations SET ts = ?1, workspace_id = COALESCE(?2, workspace_id)
                     WHERE id = (SELECT MAX(id) FROM invocations WHERE job_run_id = ?3)",
                    (at.to_rfc3339(), partition, run),
                )
                .map_err(|error| orbit_core::OrbitError::Store(error.to_string()))?;
            Ok(())
        })
        .unwrap();
}

fn outcome(fixture: &Fixture, name: &str, at: DateTime<Utc>, state: JobRunState, decision: &str) {
    let run = fixture.seed_run(name, "task_pr_pipeline", state);
    let mut pipeline = PipelineState::new(run.run_id.clone(), run.job_id.clone(), json!({}));
    pipeline.pipeline["commit"] = json!({
        "phase": "commit", "decision": decision, "committed": decision == "performed",
    });
    let store = fixture.runtime.sqlite_store().unwrap();
    let workspace = fixture.runtime.workspace_id().unwrap();
    store
        .upsert_job_run_for_workspace(&workspace, &run, Some(&pipeline))
        .unwrap();
    store
        .upsert_job_run_step_for_workspace(
            &workspace,
            &run.run_id,
            &JobRunStep {
                step_index: 0,
                target_type: JobTargetType::Activity,
                target_id: "implement_one".into(),
                started_at: Some(at - Duration::hours(1)),
                finished_at: Some(at + Duration::seconds(1)),
                duration_ms: None,
                exit_code: Some(0),
                agent_response_json: None,
                state,
                error_code: None,
                error_message: None,
                lock_wait_ms: None,
            },
        )
        .unwrap();
}

#[test]
fn implement_one_outcome_filters_every_aggregation_and_retry_attempt() {
    isolated(
        "metrics_trends::implement_one_outcome_filters_every_aggregation_and_retry_attempt",
        || {
            let fixture = Fixture::new();
            let task = add_task(&fixture, TaskComplexity::Hard, TaskStatus::Backlog);
            let at = Utc::now() - Duration::hours(2);
            record_run(&fixture, "candidate", Some(&task), 20, at, None);
            for (name, state, decision) in [
                ("failed", JobRunState::Failed, "performed"),
                ("cancelled", JobRunState::Cancelled, "performed"),
                ("interrupted", JobRunState::Interrupted, "performed"),
                ("timeout", JobRunState::Timeout, "performed"),
                ("skipped", JobRunState::Skipped, "performed"),
                ("running", JobRunState::Running, "performed"),
                ("review", JobRunState::Success, "skipped_no_diff_expected"),
                ("no-op", JobRunState::Success, "verified_no_diff"),
                ("landed", JobRunState::Success, "verified_already_landed"),
            ] {
                record_run(&fixture, name, Some(&task), 100, at, None);
                outcome(&fixture, name, at, state, decision);
            }
            // A retry can preserve the step's original start time: an earlier
            // invocation inside that same interval must not inherit its success.
            record_run(
                &fixture,
                "candidate",
                Some(&task),
                90,
                at - Duration::minutes(5),
                None,
            );
            outcome(&fixture, "candidate", at, JobRunState::Success, "performed");
            record_run(
                &fixture,
                "previous-ok",
                Some(&task),
                30,
                at - Duration::hours(30),
                None,
            );
            record_run(
                &fixture,
                "previous-review",
                Some(&task),
                100,
                at - Duration::hours(30),
                None,
            );
            outcome(
                &fixture,
                "previous-review",
                at - Duration::hours(30),
                JobRunState::Success,
                "skipped_no_diff_expected",
            );
            // Invocation-only history is unknown; a foreign successful run with the
            // same id cannot supply outcome evidence for this workspace.
            record_run(&fixture, "unknown", Some(&task), 100, at, None);
            let foreign = fixture.seed_run("unknown", "task_pr_pipeline", JobRunState::Success);
            let mut foreign_pipeline =
                PipelineState::new(foreign.run_id.clone(), foreign.job_id.clone(), json!({}));
            foreign_pipeline.pipeline["commit"] = json!({"decision":"performed", "committed":true});
            fixture
                .runtime
                .sqlite_store()
                .unwrap()
                .upsert_job_run_for_workspace("foreign", &foreign, Some(&foreign_pipeline))
                .unwrap();
            fixture.runtime.sqlite_store().unwrap().with_transaction(|tx| {
            tx.connection().execute("DELETE FROM job_run_states WHERE workspace_id = 'ws_http_fixture' AND run_id = 'unknown'", [])
                .map_err(|e| orbit_core::OrbitError::Store(e.to_string()))?;
            Ok(())
        }).unwrap();

            let server = fixture.server(false);
            let path = format!("/api/diagnostics/implement_one?since=24h&{WS}");
            let default = json_ok(server.get(&path));
            let succeeded = json_ok(server.get(&format!("{path}&outcome=succeeded")));
            for filtered in [&default, &succeeded] {
                assert_eq!(filtered["outcome"], "succeeded");
                assert_eq!(filtered["current"]["all"], stats(1, 20, 20, 20));
                assert_eq!(filtered["current"]["hard"], stats(1, 20, 20, 20));
                assert_eq!(filtered["previous"]["all"], stats(1, 30, 30, 30));
                assert_eq!(bucket_of(filtered, at)["all"]["n"], 1);
                assert_eq!(filtered["actors"][0]["n"], 1);
                assert_eq!(
                    filtered["actors"][0]["previous_avg"],
                    json!((30 * MINUTE_MS) as f64)
                );
                assert_eq!(filtered["slowest"].as_array().unwrap().len(), 1);
                assert_eq!(filtered["slowest"][0]["duration_ms"], 20 * MINUTE_MS);
                assert_eq!(filtered["implement_one_by_complexity"][0]["n"], 2);
            }
            let all = json_ok(server.get(&format!("{path}&outcome=all")));
            assert_eq!(all["outcome"], "all");
            assert_eq!(all["current"]["all"]["n"], 12);
            assert_eq!(all["previous"]["all"]["n"], 2);
            assert_eq!(
                all["buckets"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|bucket| bucket["all"]["n"].as_u64().unwrap_or(0))
                    .sum::<u64>(),
                12
            );
            assert_eq!(all["actors"][0]["n"], 12);
            assert_eq!(all["slowest"][0]["duration_ms"], 100 * MINUTE_MS);
            assert_eq!(all["implement_one_by_complexity"][0]["n"], 14);
            assert_eq!(
                server
                    .get(&format!("{path}&outcome=bogus"))
                    .status()
                    .as_u16(),
                400
            );
        },
    );
}

/// Closes `task` in the index at `at`: the row the completion trend reads.
/// The bundle's clock is not settable from outside, and archiving is not a
/// creation status, so the row is written directly.
fn close_at(fixture: &Fixture, task: &str, status: TaskStatus, at: DateTime<Utc>) {
    let index =
        rusqlite::Connection::open(fixture.global.join("tasks").join("index.sqlite")).unwrap();
    let changed = index
        .execute(
            "UPDATE task_bundle_index SET status = ?1, updated_at = ?2, terminal_month = ?3
             WHERE task_id = ?4",
            (
                status.to_string(),
                at.to_rfc3339(),
                at.format("%Y-%m").to_string(),
                task,
            ),
        )
        .unwrap();
    assert_eq!(changed, 1, "index row for {task}");
}

fn since(window: &Value) -> DateTime<Utc> {
    window["since"].as_str().unwrap().parse().unwrap()
}

fn bucket_of(response: &Value, at: DateTime<Utc>) -> &Value {
    let width = response["window"]["bucket_seconds"].as_i64().unwrap();
    let index = (at - since(&response["window"])).num_seconds() / width;
    &response["buckets"][usize::try_from(index).unwrap()]
}

fn stats(n: u64, avg: u64, p50: u64, p95: u64) -> Value {
    json!({ "n": n, "avg": (avg * MINUTE_MS) as f64, "p50": p50 * MINUTE_MS, "p95": p95 * MINUTE_MS })
}

#[test]
fn implement_one_trend_buckets_this_workspaces_runs_by_complexity() {
    isolated(
        "metrics_trends::implement_one_trend_buckets_this_workspaces_runs_by_complexity",
        || {
            let fixture = Fixture::new();
            let low = add_task(&fixture, TaskComplexity::Low, TaskStatus::Backlog);
            let medium = add_task(&fixture, TaskComplexity::Medium, TaskStatus::Backlog);
            let hard = add_task(&fixture, TaskComplexity::Hard, TaskStatus::Backlog);
            let now = Utc::now();
            let recent = now - Duration::hours(2);
            // Well inside the last hour, so a 5-minute floor never drops it.
            let unlabeled = now - Duration::minutes(50);
            record_run(&fixture, "low-a", Some(&low), 10, recent, None);
            record_run(&fixture, "low-b", Some(&low), 20, recent, None);
            record_run(
                &fixture,
                "hard",
                Some(&hard),
                60,
                now - Duration::hours(5),
                None,
            );
            record_run(&fixture, "untasked", None, 5, unlabeled, None);
            record_run(
                &fixture,
                "previous",
                Some(&medium),
                30,
                now - Duration::hours(30),
                None,
            );
            // Another workspace on the same host: never counted here.
            record_run(
                &fixture,
                "foreign",
                Some(&low),
                999,
                recent,
                Some("other-partition"),
            );

            let server = fixture.server(false);
            let day =
                json_ok(server.get(&format!("/api/diagnostics/implement_one?since=24h&{WS}")));
            assert_eq!(day["window"]["key"], "24h");
            assert_eq!(day["window"]["bucket_seconds"], 3600);
            assert_eq!(
                day["buckets"].as_array().unwrap().len(),
                25,
                "the floored start adds the partial hour"
            );
            let start = since(&day["window"]);
            let until: DateTime<Utc> = day["window"]["until"].as_str().unwrap().parse().unwrap();
            assert!(start <= until - Duration::hours(24) && start > until - Duration::hours(25));
            assert_eq!(start.timestamp() % 3600, 0, "buckets start on the hour");
            assert_eq!(day["truncated"], false);

            let current = &day["current"];
            assert_eq!(current["all"]["n"], 4);
            assert_eq!(current["low"], stats(2, 15, 10, 20));
            assert_eq!(current["medium"], Value::Null);
            assert_eq!(current["hard"]["n"], 1);
            assert_eq!(current["unset"]["n"], 1);
            assert_eq!(day["previous"]["medium"], stats(1, 30, 30, 30));
            assert_eq!(day["previous"]["all"]["n"], 1);

            let two_runs = bucket_of(&day, recent);
            assert_eq!(two_runs["low"]["n"], 2);
            assert_eq!(
                two_runs["all"]["n"], 2,
                "the foreign run is not in its bucket"
            );
            assert_eq!(two_runs["other_runs"], 0);
            let untasked = bucket_of(&day, unlabeled);
            assert_eq!(
                untasked["other_runs"], 1,
                "an unset run counts toward all only"
            );
            assert_eq!(untasked["low"], Value::Null);
            let empty = &day["buckets"][0];
            assert_eq!(empty["all"], Value::Null);

            let actors = day["actors"].as_array().unwrap();
            assert_eq!(actors.len(), 1);
            assert_eq!(actors[0]["actor"], "codex / gpt-6.1-sol");
            assert_eq!(actors[0]["n"], 4);
            assert_eq!(actors[0]["previous_avg"], json!((30 * MINUTE_MS) as f64));
            assert_eq!(actors[0]["trend"].as_array().unwrap().len(), 25);
            assert_eq!(day["slowest"][0]["duration_ms"], 60 * MINUTE_MS);
            assert_eq!(day["slowest"][0]["task_id"], hard.as_str());
            assert_eq!(day["slowest"][0]["complexity"], "hard");

            let bands: Vec<(&str, u64)> = day["implement_one_by_complexity"]
                .as_array()
                .unwrap()
                .iter()
                .map(|band| {
                    (
                        band["complexity"].as_str().unwrap(),
                        band["n"].as_u64().unwrap(),
                    )
                })
                .collect();
            assert_eq!(
                bands,
                [("unset", 1), ("low", 2), ("medium", 1), ("hard", 1)]
            );

            let all =
                json_ok(server.get(&format!("/api/diagnostics/implement_one?since=all&{WS}")));
            assert_eq!(
                all["previous"],
                Value::Null,
                "an all window has nothing before it"
            );
            assert_eq!(all["current"]["all"]["n"], 5);
            assert!(since(&all["window"]) <= now - Duration::hours(30));

            let hour =
                json_ok(server.get(&format!("/api/diagnostics/implement_one?since=1h&{WS}")));
            assert_eq!(hour["window"]["bucket_seconds"], 300);
            assert_eq!(hour["current"]["all"]["n"], 1);

            for query in ["since=bogus", "since=24h&month=2026-04"] {
                let response = server.get(&format!("/api/diagnostics/implement_one?{query}&{WS}"));
                assert_eq!(response.status().as_u16(), 400, "{query}");
            }
        },
    );
}

#[test]
fn completion_trend_counts_terminal_tasks_where_they_closed() {
    isolated(
        "metrics_trends::completion_trend_counts_terminal_tasks_where_they_closed",
        || {
            let fixture = Fixture::new();
            let now = Utc::now();
            let closed = [
                (
                    TaskComplexity::Low,
                    TaskStatus::Done,
                    now - Duration::hours(1),
                ),
                (
                    TaskComplexity::Hard,
                    TaskStatus::Done,
                    now - Duration::hours(3),
                ),
                (
                    TaskComplexity::XHard,
                    TaskStatus::Done,
                    now - Duration::hours(3),
                ),
                (
                    TaskComplexity::Medium,
                    TaskStatus::Rejected,
                    now - Duration::hours(2),
                ),
                (
                    TaskComplexity::Medium,
                    TaskStatus::Archived,
                    now - Duration::hours(30),
                ),
                (
                    TaskComplexity::Medium,
                    TaskStatus::Done,
                    now - Duration::days(40),
                ),
            ];
            for (complexity, status, at) in closed {
                let task = add_task(&fixture, complexity, TaskStatus::Done);
                close_at(&fixture, &task, status, at);
            }
            add_task(&fixture, TaskComplexity::Low, TaskStatus::Backlog);

            let server = fixture.server(false);
            let day = json_ok(server.get(&format!("/api/diagnostics/completions?since=24h&{WS}")));
            assert_eq!(day["window"]["bucket_seconds"], 3600);
            assert_eq!(
                day["current"],
                json!({
                    "done": { "total": 3, "low": 1, "medium": 0, "hard": 1, "other": 1 },
                    "rejected": 1,
                    "archived": 0,
                })
            );
            assert_eq!(day["previous"]["archived"], 1);
            assert_eq!(day["previous"]["done"]["total"], 0);
            let buckets = day["buckets"].as_array().unwrap();
            assert_eq!(buckets.len(), 25);
            let summed: u64 = buckets
                .iter()
                .map(|bucket| bucket["done"]["total"].as_u64().unwrap())
                .sum();
            assert_eq!(summed, 3, "buckets add up to the window total");
            let hard_bucket = bucket_of(&day, now - Duration::hours(3));
            assert_eq!(hard_bucket["done"]["hard"], 1);
            assert_eq!(hard_bucket["done"]["other"], 1);
            assert_eq!(bucket_of(&day, now - Duration::hours(2))["rejected"], 1);

            let all = json_ok(server.get(&format!("/api/diagnostics/completions?since=all&{WS}")));
            assert_eq!(
                all["window"]["bucket_seconds"], 86_400,
                "40 days of history in day buckets"
            );
            assert_eq!(all["previous"], Value::Null);
            assert_eq!(all["current"]["done"]["total"], 4);
            assert_eq!(all["current"]["done"]["medium"], 1);
            assert_eq!(all["current"]["archived"], 1);

            let response = server.get(&format!("/api/diagnostics/completions?since=bogus&{WS}"));
            assert_eq!(response.status().as_u16(), 400);
        },
    );
}

fn add_typed(
    fixture: &Fixture,
    task_type: TaskType,
    tags: &[&str],
    regression_from: Option<&str>,
) -> String {
    fixture
        .runtime
        .add_task(TaskAddParams {
            title: format!("{task_type:?} defect fixture"),
            description: "defect trend fixture".into(),
            task_type: Some(task_type),
            tags: tags.iter().map(|tag| tag.to_string()).collect(),
            relations: regression_from
                .map(|target| TaskRelation {
                    relation_type: TaskRelationType::RegressionFrom,
                    target: target.into(),
                })
                .into_iter()
                .collect(),
            ..Default::default()
        })
        .unwrap()
        .id
        .to_string()
}

#[test]
fn defect_trend_refreshes_types_preserved_by_older_writers() {
    isolated(
        "metrics_trends::defect_trend_refreshes_types_preserved_by_older_writers",
        || {
            let mut observed = Vec::new();
            let mut expected = Vec::new();
            for (name, from, to, cause, total, fix_for_fix, regression) in [
                (
                    "feature to bug",
                    TaskType::Feature,
                    TaskType::Bug,
                    false,
                    1,
                    0,
                    0,
                ),
                (
                    "bug to feature",
                    TaskType::Bug,
                    TaskType::Feature,
                    false,
                    0,
                    0,
                    0,
                ),
                (
                    "cause becomes bug",
                    TaskType::Feature,
                    TaskType::Bug,
                    true,
                    2,
                    1,
                    0,
                ),
                (
                    "cause becomes feature",
                    TaskType::Bug,
                    TaskType::Feature,
                    true,
                    1,
                    0,
                    1,
                ),
            ] {
                let fixture = Fixture::new();
                let task = add_typed(&fixture, from, &[], None);
                if cause {
                    add_typed(&fixture, TaskType::Bug, &[], Some(&task));
                }
                fixture
                    .runtime
                    .update_task_as_human(
                        &task,
                        TaskUpdateParams {
                            task_type: Some(to),
                            ..Default::default()
                        },
                        "fixture".into(),
                    )
                    .unwrap();

                // Warm persisted freshness proofs with the canonical type,
                // then restart the reader so its first comparison must check
                // the row fingerprint rather than an in-process envelope.
                let server = fixture.server(false);
                json_ok(server.get(&format!("/api/tasks?{WS}")));
                drop(server);

                // The pre-registry-v2 upsert updates every known column,
                // including updated_at, but preserves the existing task_type.
                // Restore exactly that mismatch without introducing NULLs.
                let index =
                    rusqlite::Connection::open(fixture.global.join("tasks").join("index.sqlite"))
                        .unwrap();
                assert_eq!(
                    index
                        .execute(
                            "UPDATE task_bundle_index SET task_type = ?1 WHERE task_id = ?2",
                            (from.to_string(), &task),
                        )
                        .unwrap(),
                    1
                );
                drop(index);

                let server = fixture.server(false);
                for _ in 0..2 {
                    let day =
                        json_ok(server.get(&format!("/api/diagnostics/defects?since=24h&{WS}")));
                    observed.push((
                        name,
                        day["current"]["total"].clone(),
                        day["current"]["lineage"]["fix_for_fix"].clone(),
                        day["current"]["lineage"]["regression"].clone(),
                    ));
                    expected.push((name, json!(total), json!(fix_for_fix), json!(regression)));
                }
            }
            assert_eq!(
                observed, expected,
                "each read uses canonical task and cause types"
            );
        },
    );
}

#[test]
fn defect_trend_classifies_bugs_by_lineage_and_origin_after_backfill() {
    isolated(
        "metrics_trends::defect_trend_classifies_bugs_by_lineage_and_origin_after_backfill",
        || {
            let fixture = Fixture::new();
            let feature = add_typed(&fixture, TaskType::Feature, &[], None);
            // A full code review's findings also carry `code-review`.
            let regression = add_typed(
                &fixture,
                TaskType::Bug,
                &["code-review", "full-code-review"],
                Some(&feature),
            );
            add_typed(
                &fixture,
                TaskType::Bug,
                &["qa-sweep-2026-09-15"],
                Some(&regression),
            );
            add_typed(&fixture, TaskType::Bug, &[], None);
            add_typed(&fixture, TaskType::Chore, &["code-review"], None);

            // Rows indexed before registry v2 have no type yet; the first read
            // must rebuild them rather than report no defects.
            let index =
                rusqlite::Connection::open(fixture.global.join("tasks").join("index.sqlite"))
                    .unwrap();
            index
                .execute("UPDATE task_bundle_index SET task_type = NULL", [])
                .unwrap();
            drop(index);

            let server = fixture.server(false);
            let day = json_ok(server.get(&format!("/api/diagnostics/defects?since=24h&{WS}")));
            let current = &day["current"];
            assert_eq!(current["total"], 3, "only bug tasks count: {day}");
            assert_eq!(
                current["lineage"],
                json!({ "fix_for_fix": 1, "regression": 1, "unattributed": 1 })
            );
            assert_eq!(current["origin"]["full-code-review"], 1);
            assert_eq!(current["origin"]["code-review"], 0);
            assert_eq!(current["origin"]["qa-sweep"], 1);
            assert_eq!(current["origin"]["other"], 1);
            let summed: u64 = day["buckets"]
                .as_array()
                .unwrap()
                .iter()
                .map(|bucket| bucket["total"].as_u64().unwrap())
                .sum();
            assert_eq!(summed, 3, "buckets add up to the window total");
        },
    );
}
