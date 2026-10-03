use chrono::Utc;
use orbit_core::application::job::JobRunListParams;
use orbit_core::application::task::TaskAddParams;
use orbit_core::{JobRunState, TaskStatus};
use orbit_types::workflow::{ChildDispatch, PipelineState};
use serde_json::json;

use super::support::{Fixture, error_code, isolated, json_ok};

#[test]
fn ship_and_resume_refuse_in_flight_duplicates_without_persisting_runs() {
    isolated(
        "workflows::ship_and_resume_refuse_in_flight_duplicates_without_persisting_runs",
        || {
            let fixture = Fixture::new();
            fixture.job("task_auto_pipeline");
            fixture.job("resume_fixture");
            let task = fixture
                .runtime
                .add_task(TaskAddParams {
                    title: "HTTP ship dedupe".into(),
                    description: "explicit ship fixture".into(),
                    status: Some(TaskStatus::Backlog),
                    ..Default::default()
                })
                .unwrap();
            let mut ship =
                fixture.seed_run("jrun-ship-live", "task_auto_pipeline", JobRunState::Pending);
            ship.input = Some(json!({"mode":"local","task_ids":[task.id]}));
            fixture.save_run(&ship);
            let source =
                fixture.seed_run("jrun-resume-source", "resume_fixture", JobRunState::Failed);
            let mut resume =
                fixture.seed_run("jrun-resume-live", "resume_fixture", JobRunState::Pending);
            resume.attempt = 2;
            resume.retry_source_run_id = Some(source.run_id.clone());
            fixture.save_run(&resume);
            let server = fixture.server(false);
            let before = fixture
                .runtime
                .list_job_runs(JobRunListParams::default())
                .unwrap();
            // Repeated clicks must continue naming the incumbent, without another receipt.
            for _ in 0..2 {
                let payload = error_code(
                    server.send(
                        "POST",
                        "/api/workflows/ship?workspace=ws_http_fixture",
                        json!({"task_ids":[task.id],"mode":"local"}),
                    ),
                    409,
                    "ship_run_in_flight",
                );
                assert_eq!(payload["run_id"], ship.run_id);
                assert_eq!(payload["task_id"], task.id);
                let payload = error_code(
                    server.send(
                        "POST",
                        &format!(
                            "/api/job-runs/{}/resume?workspace=ws_http_fixture",
                            source.run_id
                        ),
                        json!({}),
                    ),
                    409,
                    "resume_run_in_flight",
                );
                assert_eq!(payload["run_id"], resume.run_id);
                assert_eq!(payload["source_run_id"], source.run_id);
            }
            let after = fixture
                .runtime
                .list_job_runs(JobRunListParams::default())
                .unwrap();
            assert_eq!(
                after, before,
                "refused ship/resume clicks must not persist or change any run"
            );
        },
    );
}

#[test]
fn auto_stop_is_idempotent_and_preserves_in_flight_children() {
    isolated(
        "workflows::auto_stop_is_idempotent_and_preserves_in_flight_children",
        || {
            let fixture = Fixture::new();
            let coordinator =
                fixture.seed_run("jrun-auto", "workspace_auto_pipeline", JobRunState::Running);
            let child = fixture.seed_run("jrun-child", "task_auto_pipeline", JobRunState::Running);
            let mut state = PipelineState::new(
                coordinator.run_id.clone(),
                coordinator.job_id.clone(),
                json!({}),
            );
            state.record_child_dispatch(ChildDispatch::submitted(
                child.run_id.clone(),
                child.job_id.clone(),
                "invoke_detached".into(),
                false,
                false,
                Utc::now(),
            ));
            fixture
                .runtime
                .write_run_state(&coordinator.run_id, &state)
                .unwrap();
            let server = fixture.server(true);
            let first = json_ok(server.send(
                "POST",
                "/api/workflows/auto/stop?workspace=ws_http_fixture",
                json!({"reason":"first click"}),
            ));
            assert_eq!(first["outcome"], "stopped");
            assert_eq!(first["coordinators"][0]["run_id"], coordinator.run_id);
            assert_eq!(
                first["coordinators"][0]["remaining_children"][0]["run_id"],
                child.run_id
            );
            let stopped = fixture
                .runtime
                .read_run_state(&coordinator.run_id)
                .unwrap()
                .unwrap();
            assert!(stopped.admissions_stopped());
            let repeated = json_ok(server.send(
                "POST",
                "/api/workflows/auto/stop?workspace=ws_http_fixture",
                json!({"reason":"second click"}),
            ));
            assert_eq!(repeated["outcome"], "unchanged");
            assert_eq!(repeated["coordinators"][0]["outcome"], "unchanged");
            assert_eq!(
                repeated["coordinators"][0]["remaining_children"],
                first["coordinators"][0]["remaining_children"]
            );
            assert_eq!(
                fixture
                    .runtime
                    .read_run_state(&coordinator.run_id)
                    .unwrap()
                    .unwrap(),
                stopped,
                "a repeated stop preserves the first stop's evidence"
            );
            for run in [&coordinator, &child] {
                let detail = json_ok(server.get(&format!(
                    "/api/runs/{}?workspace=ws_http_fixture",
                    run.run_id
                )));
                assert_eq!(
                    detail["run"]["state"], "running",
                    "stopping admissions must not cancel a run: {detail}"
                );
            }
        },
    );
}
