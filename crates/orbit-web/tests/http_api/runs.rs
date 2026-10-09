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
