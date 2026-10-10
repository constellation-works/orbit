use std::collections::BTreeSet;

use chrono::Utc;
use orbit_core::JobRunState;
use orbit_core::application::task::TaskAddParams;
use orbit_types::task::is_valid_orb_task_id;
use orbit_types::workflow::{ChildDispatch, PipelineState};
use serde_json::{Value, json};

use super::support::{Fixture, isolated, json_ok};

fn operations(trace: &[Value], name: &str) -> usize {
    trace
        .iter()
        .filter(|event| event["span"]["name"] == name)
        .count()
}

#[test]
fn child_dispatches_show_live_run_outcomes_and_timing_over_http() {
    isolated(
        "runs::child_dispatches_show_live_run_outcomes_and_timing_over_http",
        || {
            let fixture = Fixture::new();
            let parent = fixture.seed_run(
                "jrun-child-outcomes",
                "workspace_auto_pipeline",
                JobRunState::Success,
            );
            let mut pipeline =
                PipelineState::new(parent.run_id.clone(), parent.job_id.clone(), json!({}));
            let children = [
                JobRunState::Running,
                JobRunState::Success,
                JobRunState::Failed,
                JobRunState::Cancelled,
                JobRunState::Pending,
                JobRunState::Retrying,
                JobRunState::Timeout,
                JobRunState::Interrupted,
                JobRunState::Held,
                JobRunState::Skipped,
            ]
            .map(|state| {
                let mut child =
                    fixture.seed_run(&format!("jrun-child-{state}"), "task_auto_pipeline", state);
                child.started_at = (state != JobRunState::Pending)
                    .then_some(Utc::now() - chrono::Duration::seconds(120));
                child.duration_ms = state.is_terminal().then_some(120_000);
                fixture.save_run(&child);
                let mut dispatch = ChildDispatch::submitted(
                    child.run_id.clone(),
                    child.job_id.clone(),
                    "leaf_invoke".into(),
                    false,
                    false,
                    Utc::now(),
                );
                dispatch.child_status = Some("running".into());
                pipeline.record_child_dispatch(dispatch);
                child
            });
            pipeline.record_child_dispatch(ChildDispatch::submitted(
                "jrun-child-unavailable".into(),
                "task_auto_pipeline".into(),
                "leaf_invoke".into(),
                false,
                false,
                Utc::now(),
            ));
            fixture
                .runtime
                .write_run_state(&parent.run_id, &pipeline)
                .unwrap();
            let server = fixture.server(false);
            let detail = json_ok(server.get(&format!("/api/runs/{}", parent.run_id)));
            let dispatches = detail["run"]["child_dispatches"].as_array().unwrap();
            assert_eq!(dispatches.len(), children.len() + 1);
            for (dispatch, child) in dispatches.iter().zip(&children) {
                let stored = fixture.runtime.show_job_run(&child.run_id).unwrap();
                assert_eq!(dispatch["child_run_id"], stored.run_id);
                assert_eq!(dispatch["state"], json!(stored.state));
                assert_eq!(dispatch["started_at"], json!(stored.started_at));
                assert_eq!(dispatch["finished_at"], json!(stored.finished_at));
                assert_eq!(dispatch["duration_ms"], json!(stored.duration_ms));
                assert_eq!(dispatch["phase"], "submitted");
                assert_eq!(dispatch["child_status"], "running");
            }
            let unavailable = dispatches.last().unwrap();
            for field in ["state", "started_at", "finished_at", "duration_ms", "tasks"] {
                assert!(unavailable.get(field).unwrap().is_null(), "{field}");
            }

            // The parent's durable checkpoint never changed, even after the
            // child finishes. A fresh HTTP read must still see its new state.
            let mut finished = children[0].clone();
            finished.state = JobRunState::Success;
            finished.finished_at = Some(Utc::now());
            finished.duration_ms = Some(123_000);
            fixture.save_run(&finished);
            let refreshed = json_ok(server.get(&format!("/api/runs/{}", parent.run_id)));
            let dispatch = &refreshed["run"]["child_dispatches"][0];
            assert_eq!(dispatch["state"], "success");
            assert_eq!(dispatch["duration_ms"], 123_000);
            assert_eq!(dispatch["finished_at"], json!(finished.finished_at));
            assert_eq!(dispatch["phase"], "submitted");
            assert_eq!(dispatch["child_status"], "running");
        },
    );
}

#[test]
fn run_tasks_are_projected_over_http_without_per_row_task_reads() {
    isolated(
        "runs::run_tasks_are_projected_over_http_without_per_row_task_reads",
        || {
            let fixture = Fixture::new();
            let tasks = ["First delivered task", "Second delivered task"].map(|title| {
                fixture
                    .runtime
                    .add_task(TaskAddParams {
                        title: title.into(),
                        description: "run projection fixture".into(),
                        ..Default::default()
                    })
                    .unwrap()
            });
            let expected = json!([
                {"id":tasks[0].id,"title":tasks[0].title},
                {"id":tasks[1].id,"title":tasks[1].title},
            ]);
            for index in 0..30 {
                let job = [
                    "task_pr_pipeline",
                    "task_gate_pipeline",
                    "task_auto_pipeline",
                ][index % 3];
                let mut run =
                    fixture.seed_run(&format!("jrun-task-{index}"), job, JobRunState::Success);
                run.input =
                    Some(json!({"task_ids":[tasks[1].id, tasks[0].id, tasks[0].id, "", 5]}));
                fixture.save_run(&run);
            }
            let mut singular =
                fixture.seed_run("jrun-singular", "task_pr_pipeline", JobRunState::Failed);
            singular.input = Some(json!({"task_id":tasks[0].id}));
            fixture.save_run(&singular);
            let mut missing = fixture.seed_run(
                "jrun-missing-title",
                "task_pr_pipeline",
                JobRunState::Success,
            );
            missing.input = Some(json!({"task_ids":["HF-999999", "not a task id"]}));
            fixture.save_run(&missing);
            let plain = fixture.seed_run("jrun-plain", "maintenance", JobRunState::Success);
            let mut coordinator = fixture.seed_run(
                "jrun-coordinator",
                "workspace_auto_pipeline",
                JobRunState::Success,
            );
            coordinator.input = Some(json!({"task_ids":[]}));
            fixture.save_run(&coordinator);
            let mut state = PipelineState::new(
                coordinator.run_id.clone(),
                coordinator.job_id.clone(),
                json!({}),
            );
            state.record_child_dispatch(ChildDispatch::submitted(
                "jrun-task-0".into(),
                "task_pr_pipeline".into(),
                "dispatch".into(),
                false,
                true,
                Utc::now(),
            ));
            fixture
                .runtime
                .write_run_state(&coordinator.run_id, &state)
                .unwrap();
            let server = fixture.counted_task_server();
            for limit in [1, 25, 100] {
                let before = server.task_query_trace().len();
                let page = json_ok(server.get(&format!(
                    "/api/job-runs?job_id=task_pr_pipeline&limit={limit}"
                )));
                let trace = server.task_query_trace();
                let reads = &trace[before..];
                let referenced: BTreeSet<&str> = page["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .flat_map(|row| row["task_ids"].as_array().into_iter().flatten())
                    .filter_map(Value::as_str)
                    .filter(|id| is_valid_orb_task_id(id))
                    .collect();
                assert_eq!(
                    operations(reads, "task_index_freshness"),
                    0,
                    "labels never validate the workspace task index: {reads:?}"
                );
                assert_eq!(
                    operations(reads, "task_metadata_read"),
                    referenced.len(),
                    "one keyed read per task the page references, not per workspace task: {reads:?}"
                );
                assert_eq!(
                    operations(reads, "task_bundle_materialization"),
                    0,
                    "run lists never hydrate task bodies"
                );
                assert_eq!(page["items"].as_array().unwrap().len(), limit.min(12));
            }
            let page = json_ok(server.get("/api/job-runs?limit=100"));
            let aggregate = json_ok(server.get("/api/job-runs/all?limit=100"));
            for rows in [&page["items"], &aggregate["items"]] {
                for row in rows.as_array().unwrap() {
                    let id = row["run_id"].as_str().unwrap();
                    let expected_tasks = match id {
                        "jrun-plain" | "jrun-coordinator" => Value::Null,
                        "jrun-singular" => json!([{"id":tasks[0].id,"title":tasks[0].title}]),
                        "jrun-missing-title" => json!([
                            {"id":"HF-999999","title":null},
                            {"id":"not a task id","title":null},
                        ]),
                        _ => expected.clone(),
                    };
                    assert_eq!(row["tasks"], expected_tasks, "list task labels for {id}");
                    let ids = expected_tasks.as_array().map(|tasks| {
                        tasks
                            .iter()
                            .map(|task| task["id"].clone())
                            .collect::<Vec<_>>()
                    });
                    assert_eq!(row["task_ids"], json!(ids));
                    let detail = json_ok(server.get(&format!("/api/runs/{id}")));
                    assert_eq!(
                        detail["run"]["tasks"], row["tasks"],
                        "detail labels for {id}"
                    );
                    assert_eq!(detail["run"]["task_ids"], row["task_ids"]);
                    if id == coordinator.run_id {
                        assert_eq!(detail["run"]["child_dispatches"][0]["tasks"], expected);
                    }
                }
            }
            let before = server.task_query_trace().len();
            let page = json_ok(server.get("/api/job-runs?job_id=maintenance"));
            assert_eq!(page["items"][0]["run_id"], plain.run_id);
            assert_eq!(
                server.task_query_trace().len(),
                before,
                "task-free lists need no metadata lookup"
            );
        },
    );
}

#[test]
fn job_run_list_filters_by_task_job_and_since() {
    isolated("runs::job_run_list_filters_by_task_job_and_since", || {
        let fixture = Fixture::new();
        let seed =
            |name: &str, job: &str, state: JobRunState, input: Option<Value>, age_hours: i64| {
                let mut run = fixture.seed_run(name, job, state);
                run.input = input;
                let created = Utc::now() - chrono::Duration::hours(age_hours);
                run.created_at = created;
                run.scheduled_at = created;
                if run.started_at.is_some() {
                    run.started_at = Some(created);
                }
                if run.finished_at.is_some() {
                    run.finished_at = Some(created);
                }
                fixture.save_run(&run);
            };
        seed(
            "jrun-a",
            "task_auto_pipeline",
            JobRunState::Failed,
            Some(json!({"task_ids": ["ORB-10001"]})),
            0,
        );
        seed(
            "jrun-b",
            "task_gate_pipeline",
            JobRunState::Timeout,
            Some(json!({"task_id": "ORB-10001"})),
            0,
        );
        seed(
            "jrun-c",
            "task_auto_pipeline",
            JobRunState::Interrupted,
            Some(json!({"task_ids": ["ORB-10002"]})),
            0,
        );
        seed(
            "jrun-d",
            "ci_failure_sweep_pipeline",
            JobRunState::Failed,
            Some(json!({"task_ids": ["ORB-10001"]})),
            0,
        );
        seed(
            "jrun-e",
            "task_auto_pipeline",
            JobRunState::Success,
            Some(json!({"task_ids": ["ORB-10001"]})),
            0,
        );
        seed(
            "jrun-f",
            "task_auto_pipeline",
            JobRunState::Failed,
            Some(json!({"task_ids": ["ORB-10001"]})),
            48,
        );
        seed("jrun-g", "maintenance", JobRunState::Failed, None, 0);
        seed(
            "jrun-h",
            "task_auto_pipeline",
            JobRunState::Failed,
            Some(json!({"wrapper": {"task_id": "ORB-10001", "task_ids": ["ORB-10001"]}})),
            0,
        );
        let server = fixture.server(false);
        let ids = |page: &Value| {
            page["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|run| run["run_id"].as_str().unwrap().to_string())
                .collect::<BTreeSet<_>>()
        };
        let workspace = "workspace=ws_http_fixture";

        let task = json_ok(server.get(&format!(
            "/api/job-runs?task_id=ORB-10001&limit=100&{workspace}"
        )));
        assert_eq!(
            ids(&task),
            BTreeSet::from([
                "jrun-a".to_string(),
                "jrun-b".to_string(),
                "jrun-d".to_string(),
                "jrun-e".to_string(),
                "jrun-f".to_string(),
            ]),
            "task_id matches top-level task_ids or task_id only"
        );
        assert_eq!(task["total"], 5);

        let recent_failures = json_ok(server.get(&format!(
            "/api/job-runs?task_id=ORB-10001&state=failed&since=24h&limit=100&{workspace}"
        )));
        assert_eq!(
            ids(&recent_failures),
            BTreeSet::from([
                "jrun-a".to_string(),
                "jrun-b".to_string(),
                "jrun-d".to_string(),
            ])
        );
        assert_eq!(recent_failures["state"], "failed");

        let job = json_ok(server.get(&format!(
            "/api/job-runs?job_id=ci_failure_sweep_pipeline&limit=100&{workspace}"
        )));
        assert_eq!(ids(&job), BTreeSet::from(["jrun-d".to_string()]));

        let both = json_ok(server.get(&format!(
                "/api/job-runs?task_id=ORB-10001&job_id=task_auto_pipeline&state=failed&since=24h&limit=100&{workspace}"
            )));
        assert_eq!(ids(&both), BTreeSet::from(["jrun-a".to_string()]));

        let unbounded = json_ok(server.get(&format!(
            "/api/job-runs?task_id=ORB-10001&since=all&limit=100&{workspace}"
        )));
        assert_eq!(ids(&unbounded), ids(&task), "since=all is not a time bound");

        let blank_task = json_ok(server.get(&format!(
            "/api/job-runs?task_id=%20&state=failed&since=24h&limit=100&{workspace}"
        )));
        let failed_window = json_ok(server.get(&format!(
            "/api/job-runs?state=failed&since=24h&limit=100&{workspace}"
        )));
        assert_eq!(
            ids(&failed_window),
            BTreeSet::from([
                "jrun-a".to_string(),
                "jrun-b".to_string(),
                "jrun-c".to_string(),
                "jrun-d".to_string(),
                "jrun-g".to_string(),
                "jrun-h".to_string(),
            ])
        );
        assert_eq!(blank_task["total"], failed_window["total"]);
        assert_eq!(failed_window["total"], 6);

        let summary = json_ok(server.get("/api/audit/summary?since=24h&workspace=ws_http_fixture"));
        assert_eq!(
            summary["failed_runs"], failed_window["total"],
            "the rail count and the Failed list share one window and outcome set"
        );

        let aggregate = json_ok(
            server.get("/api/job-runs/all?task_id=ORB-10001&state=failed&since=24h&limit=100"),
        );
        assert_eq!(
            ids(&aggregate),
            BTreeSet::from([
                "jrun-a".to_string(),
                "jrun-b".to_string(),
                "jrun-d".to_string(),
            ])
        );

        let refused = server.get(&format!("/api/job-runs?since=not-a-window&{workspace}"));
        assert_eq!(refused.status().as_u16(), 400);
        let body: Value = refused.json().expect("error json");
        let message = body["error"].as_str().expect("error text");
        assert!(message.contains("not-a-window"), "{message}");
        assert!(body.get("code").is_none(), "{body}");
        assert_eq!(
            server
                .get("/api/job-runs/all?since=not-a-window")
                .status()
                .as_u16(),
            400
        );
    });
}
