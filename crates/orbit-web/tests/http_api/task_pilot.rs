use orbit_core::application::job::JobRunListParams;
use orbit_core::application::task::TaskAddParams;
use orbit_core::{JobRunState, TaskStatus};
use serde_json::json;

use super::support::{Fixture, error_code, isolated};

/// Run pilot refuses every case the pipeline's explicit mode would not assess,
/// and an unauthorized session, before any run exists or any task changes.
#[test]
fn run_pilot_refuses_without_submitting_or_changing_the_task() {
    isolated(
        "task_pilot::run_pilot_refuses_without_submitting_or_changing_the_task",
        || {
            let fixture = Fixture::new();
            fixture.job("task_pilot_pipeline");
            fixture.job("task_auto_pipeline");
            let add = |title: &str, status: TaskStatus| {
                fixture
                    .runtime
                    .add_task(TaskAddParams {
                        title: title.into(),
                        description: "run pilot refusal fixture".into(),
                        status: Some(status),
                        ..Default::default()
                    })
                    .unwrap()
            };
            let held = add("held by a pilot", TaskStatus::Proposed);
            let admitted = add("admitted to delivery", TaskStatus::Backlog);
            let retired = add("retired", TaskStatus::Proposed);
            fixture.runtime.archive_task(&retired.id).unwrap();
            let mut pilot = fixture.seed_run(
                "jrun-pilot-live",
                "task_pilot_pipeline",
                JobRunState::Pending,
            );
            pilot.input = Some(json!({"task_ids": [held.id]}));
            fixture.save_run(&pilot);
            let mut ship =
                fixture.seed_run("jrun-ship-live", "task_auto_pipeline", JobRunState::Pending);
            ship.input = Some(json!({"mode": "local", "task_ids": [admitted.id]}));
            fixture.save_run(&ship);

            let operator = fixture.server(true);
            let before = fixture
                .runtime
                .list_job_runs(JobRunListParams::default())
                .unwrap();
            let pilot_uri = |id: &str| format!("/api/tasks/{id}/pilot?workspace=ws_http_fixture");

            let payload = error_code(
                operator.send("POST", &pilot_uri(&held.id), json!({})),
                409,
                "task_pilot_already_running",
            );
            assert!(
                payload["error"]
                    .as_str()
                    .unwrap()
                    .contains("jrun-pilot-live")
            );
            error_code(
                operator.send("POST", &pilot_uri(&admitted.id), json!({})),
                409,
                "task_pilot_delivery_running",
            );
            error_code(
                operator.send("POST", &pilot_uri(&retired.id), json!({})),
                409,
                "task_pilot_unsupported_status",
            );
            assert_eq!(
                operator
                    .send("POST", &pilot_uri("ORB-99999"), json!({}))
                    .status()
                    .as_u16(),
                404
            );
            error_code(
                fixture
                    .server(false)
                    .send("POST", &pilot_uri(&held.id), json!({})),
                403,
                "authorization_denied",
            );

            assert_eq!(
                fixture
                    .runtime
                    .list_job_runs(JobRunListParams::default())
                    .unwrap(),
                before,
                "a refused Run pilot must not persist a run"
            );
            for (task, status) in [
                (&held, TaskStatus::Proposed),
                (&admitted, TaskStatus::Backlog),
                (&retired, TaskStatus::Archived),
            ] {
                assert_eq!(fixture.runtime.get_task(&task.id).unwrap().status, status);
            }

            // A submission renders the shared core result under the dashboard's
            // names, now with its queue position [ORB-22908]. The seeded live
            // pilot run holds the job's slot, so this one waits behind it.
            let fresh = add("fresh for a pilot", TaskStatus::Proposed);
            let response = operator.send("POST", &pilot_uri(&fresh.id), json!({}));
            assert_eq!(response.status().as_u16(), 200);
            let submitted: serde_json::Value = response.json().unwrap();
            assert_eq!(submitted["task_id"], json!(fresh.id), "{submitted}");
            assert_eq!(submitted["job_id"], "task_pilot_pipeline", "{submitted}");
            assert_eq!(submitted["state"], "queued", "{submitted}");
            assert!(submitted["run_id"].is_string(), "{submitted}");
            assert!(submitted["submitted_at"].is_string(), "{submitted}");
            assert_eq!(submitted["queue_position"], 1, "{submitted}");
        },
    );
}
