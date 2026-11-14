//! Closed-task analytics date a task by its durable terminal transition, so a
//! later write never moves it, and a task without one stays visibly undated.

use chrono::{DateTime, Duration, Utc};
use orbit_core::application::task::TaskUpdateParams;
use orbit_store::contracts::{InvocationInsertParams, TaskArtifactUpdateParams};
use orbit_types::task::{ArtifactWriter, TaskArtifact, TaskComplexity, TaskStatus};
use orbit_types::telemetry::{InvocationTrace, TokenUsage, ToolCallTrace};
use serde_json::Value;

use super::backlog_forecast::comment_on_task;
use super::metrics_trends::{
    add_task, bucket_of, close_at, index_terminal_row, rewrite_task_events, task_events_path,
};
use super::support::{Fixture, isolated, json_ok};

const WS: &str = "workspace=ws_http_fixture";

/// A task moved to done through the public update path, then dated at `at`.
fn complete_at(fixture: &Fixture, at: DateTime<Utc>) -> String {
    let task = add_task(fixture, TaskComplexity::Low, TaskStatus::Review);
    fixture
        .runtime
        .update_task_as_human(
            &task,
            TaskUpdateParams {
                status: Some(TaskStatus::Done),
                execution_summary: Some("Delivered by the fixture".into()),
                ..Default::default()
            },
            "fixture".into(),
        )
        .unwrap();
    close_at(fixture, &task, TaskStatus::Done, at);
    task
}

/// A done task the index last saw written at `last_write`, whose history
/// records no entry into done.
fn done_without_transition(fixture: &Fixture, last_write: DateTime<Utc>) -> String {
    let task = add_task(fixture, TaskComplexity::Low, TaskStatus::Done);
    rewrite_task_events(fixture, &task, |events| {
        for event in events.iter_mut() {
            event.to_status = None;
        }
    });
    index_terminal_row(fixture, &task, TaskStatus::Done, last_write);
    task
}

/// The maintenance writes that used to re-date a closed task: a crew backfill,
/// a comment and an artifact, through the real writers.
fn write_metadata_later(fixture: &Fixture, task: &str) {
    fixture
        .runtime
        .update_task_as_human(
            task,
            TaskUpdateParams {
                crew: Some(Some("sol".into())),
                ..Default::default()
            },
            "fixture".into(),
        )
        .unwrap();
    comment_on_task(fixture, task);
    fixture
        .runtime
        .stores()
        .task_artifacts()
        .upsert_task_artifacts(
            task,
            TaskArtifactUpdateParams {
                actor: "system".into(),
                writer: Some(ArtifactWriter::System),
                upsert_artifacts: vec![TaskArtifact::from_text("later.txt", "evidence")],
                ..Default::default()
            },
        )
        .unwrap();
}

fn assert_index_moved_to_now(fixture: &Fixture, tasks: &[&str], now: DateTime<Utc>) {
    let rows = fixture
        .runtime
        .task_terminal_transitions(Some(now))
        .unwrap();
    for task in tasks {
        assert!(
            rows.iter()
                .any(|row| row.task_id == **task && row.at >= now),
            "the index keeps dating {task} by its last write"
        );
    }
}

/// One linked invocation with measured tokens and a first edit, stamped at `at`.
fn record_delivery(fixture: &Fixture, task: &str, at: DateTime<Utc>) {
    let run = format!("delivery-{task}");
    fixture
        .runtime
        .insert_invocation_trace_record(&InvocationInsertParams {
            agent_execution_id: None,
            job_run_id: run.clone(),
            account: None,
            activity_id: "implement_one".into(),
            agent: "codex".into(),
            provider: None,
            model: Some("gpt-6.1-sol".into()),
            task_ids: vec![task.to_string()],
            trace: InvocationTrace {
                usage: TokenUsage {
                    input: 100,
                    ..Default::default()
                },
                tool_calls: vec![ToolCallTrace {
                    seq: 1,
                    tool_name: "Edit".to_string(),
                    result_bytes: 10,
                    ..Default::default()
                }],
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
                    "UPDATE invocations SET ts = ?1 WHERE job_run_id = ?2",
                    (at.to_rfc3339(), &run),
                )
                .map_err(|error| orbit_core::OrbitError::Store(error.to_string()))?;
            Ok(())
        })
        .unwrap();
}

#[test]
fn completion_trend_keeps_tasks_at_their_durable_transition() {
    isolated(
        "metrics_terminal_dates::completion_trend_keeps_tasks_at_their_durable_transition",
        || {
            let fixture = Fixture::new();
            let now = Utc::now();
            let closed_at = now - Duration::hours(3);
            let in_window = complete_at(&fixture, closed_at);
            let in_previous = complete_at(&fixture, now - Duration::hours(30));
            let before_both = complete_at(&fixture, now - Duration::days(40));
            let archived = add_task(&fixture, TaskComplexity::Low, TaskStatus::Done);
            close_at(
                &fixture,
                &archived,
                TaskStatus::Archived,
                now - Duration::hours(2),
            );
            // Undated, three ways: no entry into done, an unreadable log, and
            // a last write that rules out both windows.
            done_without_transition(&fixture, now - Duration::hours(2));
            let unreadable = done_without_transition(&fixture, now - Duration::hours(2));
            std::fs::remove_file(task_events_path(&fixture, &unreadable)).unwrap();
            let stale = done_without_transition(&fixture, now - Duration::days(40));

            let server = fixture.server(false);
            let path = format!("/api/diagnostics/completions?since=24h&{WS}");
            let before = json_ok(server.get(&path));
            assert_eq!(before["current"]["done"]["total"], 1);
            assert_eq!(before["current"]["archived"], 1);
            assert_eq!(before["previous"]["done"]["total"], 1);
            assert_eq!(bucket_of(&before, closed_at)["done"]["low"], 1);
            assert_eq!(before["undated"]["done"]["total"], 2);
            assert_eq!(before["previous_undated"]["done"]["total"], 2);

            for task in [&in_window, &in_previous, &before_both] {
                write_metadata_later(&fixture, task);
            }
            assert_index_moved_to_now(&fixture, &[&in_window, &in_previous, &before_both], now);
            // A later write widens what an undated task could be, but it still
            // has no time to count at.
            comment_on_task(&fixture, &stale);

            // The Metrics trends memo serves the first response for 90 s, so
            // the post-write read runs on a fresh server with a cold memo.
            let server = fixture.server(false);
            let after = json_ok(server.get(&path));
            assert_eq!(after["current"], before["current"], "window totals");
            assert_eq!(after["previous"], before["previous"], "previous totals");
            assert_eq!(bucket_of(&after, closed_at)["done"]["total"], 1);
            let newest = after["buckets"].as_array().unwrap().last().unwrap();
            assert_eq!(newest["done"]["total"], 0, "no delivery happened just now");
            assert_eq!(after["undated"]["done"]["total"], 3);

            let all = json_ok(server.get(&format!("/api/diagnostics/completions?since=all&{WS}")));
            assert_eq!(all["current"]["done"]["total"], 3);
            assert_eq!(all["undated"]["done"]["total"], 3);
            assert_eq!(all["previous_undated"], Value::Null);
            let opened: DateTime<Utc> = all["window"]["since"].as_str().unwrap().parse().unwrap();
            assert!(
                opened <= now - Duration::days(39),
                "`all` opens at the oldest completion, not its later write: {opened}"
            );
        },
    );
}

#[test]
fn closed_task_populations_ignore_later_writes_and_report_undated_tasks() {
    isolated(
        "metrics_terminal_dates::closed_task_populations_ignore_later_writes_and_report_undated_tasks",
        || {
            let fixture = Fixture::new();
            let now = Utc::now();
            let member = complete_at(&fixture, now - Duration::hours(3));
            record_delivery(&fixture, &member, now - Duration::hours(4));
            let outsider = complete_at(&fixture, now - Duration::days(40));
            record_delivery(&fixture, &outsider, now - Duration::hours(4));
            done_without_transition(&fixture, now - Duration::hours(2));

            let server = fixture.server(false);
            let tokens = format!("/api/diagnostics/task_tokens?since=24h&{WS}");
            let turns = format!("/api/diagnostics/turns_to_edit?since=24h&{WS}");
            let friction =
                format!("/api/diagnostics/delivery_friction?since=24h&families=tokens&{WS}");
            let read = || {
                (
                    json_ok(server.get(&tokens)),
                    json_ok(server.get(&turns)),
                    json_ok(server.get(&friction)),
                )
            };

            let check = |(tokens, turns, friction): &(Value, Value, Value)| {
                assert_eq!(tokens["total"]["n"], 1, "tokens population: {tokens}");
                assert_eq!(tokens["total"]["excluded_no_invocations"], 0);
                assert_eq!(tokens["undated_tasks"], 1);
                assert_eq!(turns["coverage"]["tasks"], 1, "first-edit population");
                assert_eq!(turns["coverage"]["measured"], 1);
                assert_eq!(turns["undated_tasks"], 1);
                assert_eq!(friction["done"]["total"], 1, "friction denominator");
                assert_eq!(friction["done"]["unfiltered_total"], 1);
                assert_eq!(friction["done"]["undated"], 1);
                assert_eq!(friction["done"]["unfiltered_undated"], 1);
                let total = &friction["families"][0]["total"];
                assert_eq!(total["denominator"], 1);
                let count = total["count"].as_f64().unwrap();
                assert!(count > 0.0);
                assert_eq!(
                    total["rate"].as_f64(),
                    Some(count),
                    "the rate divides by one done task"
                );
            };
            let before = read();
            check(&before);

            write_metadata_later(&fixture, &member);
            write_metadata_later(&fixture, &outsider);
            assert_index_moved_to_now(&fixture, &[&member, &outsider], now);

            let after = read();
            check(&after);
            assert_eq!(after.0["total"], before.0["total"], "token cohort");
            assert_eq!(
                after.1["coverage"], before.1["coverage"],
                "first-edit cohort"
            );
            assert_eq!(after.2["done"]["total"], before.2["done"]["total"]);
            assert_eq!(
                after.2["families"][0]["total"], before.2["families"][0]["total"],
                "friction rate"
            );
        },
    );
}

fn history_reads(trace: &[Value]) -> usize {
    trace
        .iter()
        .filter(|event| event["span"]["name"] == "task_history_read")
        .count()
}

/// ORB-22423: the completion trend read every candidate's history on every
/// request. A repeat request reuses each clean read while the task's bundle
/// is unchanged, and a public write to a task dates it from history again.
#[test]
fn completion_trend_rereads_only_tasks_written_since_the_last_request() {
    isolated(
        "metrics_terminal_dates::completion_trend_rereads_only_tasks_written_since_the_last_request",
        || {
            let fixture = Fixture::new();
            let now = Utc::now();
            let reconsidered = add_task(&fixture, TaskComplexity::Low, TaskStatus::Rejected);
            close_at(
                &fixture,
                &reconsidered,
                TaskStatus::Rejected,
                now - Duration::hours(30),
            );
            let commented_at = now - Duration::hours(3);
            let commented = complete_at(&fixture, commented_at);
            let unreadable = done_without_transition(&fixture, now - Duration::hours(2));
            std::fs::remove_file(task_events_path(&fixture, &unreadable)).unwrap();

            let server = fixture.counted_task_server();
            let path = format!("/api/diagnostics/completions?since=24h&{WS}");
            let request = || {
                let before = server.task_query_trace().len();
                let body = json_ok(server.get(&path));
                (body, history_reads(&server.task_query_trace()[before..]))
            };

            let (cold, reads) = request();
            assert_eq!(reads, 3, "the first request reads each candidate");
            assert_eq!(cold["current"]["done"]["total"], 1);
            assert_eq!(cold["previous"]["rejected"], 1);
            assert_eq!(cold["undated"]["done"]["total"], 1);

            let (warm, reads) = request();
            assert_eq!(
                reads, 0,
                "the Metrics trends memo serves the repeat request without rereading"
            );
            for key in ["current", "previous", "undated", "previous_undated"] {
                assert_eq!(warm[key], cold[key], "{key}");
            }

            // A distinct window still computes, and reuses each clean history
            // read while the task's bundle is unchanged: only the unreadable
            // history is read again, so it stays undated.
            let wider = format!("/api/diagnostics/completions?since=7d&{WS}");
            let before = server.task_query_trace().len();
            let week = json_ok(server.get(&wider));
            assert_eq!(
                history_reads(&server.task_query_trace()[before..]),
                1,
                "only the unreadable history is read again"
            );
            assert_eq!(week["undated"]["done"]["total"], 1);

            // Reconsidering the rejection and rejecting again enters it anew
            // now; a comment on the other task moves its log stamp, not its date.
            for status in [TaskStatus::Backlog, TaskStatus::Rejected] {
                fixture
                    .runtime
                    .update_task_as_human(
                        &reconsidered,
                        TaskUpdateParams {
                            status: Some(status),
                            ..Default::default()
                        },
                        "fixture".into(),
                    )
                    .unwrap();
            }
            comment_on_task(&fixture, &commented);

            // The memo would serve the pre-write response for 90 s, so the
            // post-write read runs on a fresh server with a cold memo.
            let server = fixture.counted_task_server();
            let request = || {
                let before = server.task_query_trace().len();
                let body = json_ok(server.get(&path));
                (body, history_reads(&server.task_query_trace()[before..]))
            };
            let (written, reads) = request();
            assert_eq!(reads, 3, "both written tasks and the unreadable one");
            assert_eq!(written["previous"]["rejected"], 0);
            assert_eq!(written["current"]["rejected"], 1, "rejected again just now");
            assert_eq!(written["current"]["done"]["total"], 1);
            assert_eq!(bucket_of(&written, commented_at)["done"]["total"], 1);
            assert_eq!(written["undated"]["done"]["total"], 1);
        },
    );
}
