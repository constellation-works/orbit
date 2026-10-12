//! Health › Metrics trends [ORB-15746]: implement_one durations and terminal
//! task transitions, bucketed on the clock and scoped to the workspace.

use chrono::{DateTime, Duration, Utc};
use orbit_core::application::task::TaskAddParams;
use orbit_store::contracts::InvocationInsertParams;
use orbit_types::task::{TaskComplexity, TaskStatus};
use orbit_types::telemetry::InvocationTrace;
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
                     WHERE job_run_id = ?3",
                    (at.to_rfc3339(), partition, run),
                )
                .map_err(|error| orbit_core::OrbitError::Store(error.to_string()))?;
            Ok(())
        })
        .unwrap();
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
            assert!(start <= now - Duration::hours(24) && start > now - Duration::hours(25));
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
