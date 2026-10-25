use orbit_core::TaskStatus;
use orbit_core::application::job::JobRunListParams;
use orbit_core::application::task::TaskAddParams;
use serde_json::{Value, json};

use super::support::{Fixture, error_code, isolated};

/// Fix PR answers its readiness and refuses a start with a typed code, and an
/// unauthorized session neither reads the forge nor starts anything.
#[test]
fn fix_pr_refuses_with_typed_codes_without_starting_a_run() {
    isolated(
        "task_auto_fix::fix_pr_refuses_with_typed_codes_without_starting_a_run",
        || {
            let fixture = Fixture::new();
            let task = fixture
                .runtime
                .add_task(TaskAddParams {
                    title: "in review without a delivery".into(),
                    description: "fix PR refusal fixture".into(),
                    status: Some(TaskStatus::Review),
                    ..Default::default()
                })
                .unwrap();
            let uri = |id: &str| format!("/api/tasks/{id}/auto-fix?workspace=ws_http_fixture");
            let before = fixture
                .runtime
                .list_job_runs(JobRunListParams::default())
                .unwrap();

            let operator = fixture.server(true);
            let readiness: Value = operator.get(&uri(&task.id)).json().unwrap();
            assert_eq!(readiness["authorized"], true, "{readiness}");
            assert_eq!(readiness["available"], false, "{readiness}");
            assert_eq!(readiness["reason"], "no_delivery_leaf", "{readiness}");
            let payload = error_code(
                operator.send("POST", &uri(&task.id), json!({})),
                409,
                "task_auto_fix_no_delivery_leaf",
            );
            assert_eq!(payload["reason"], "no_delivery_leaf");
            assert_eq!(
                operator
                    .send("POST", &uri("ORB-99999"), json!({}))
                    .status()
                    .as_u16(),
                404
            );

            let agent = fixture.server(false);
            let readiness: Value = agent.get(&uri(&task.id)).json().unwrap();
            assert_eq!(readiness["authorized"], false, "{readiness}");
            assert_eq!(readiness["reason"], "not_authorized", "{readiness}");
            let payload = error_code(
                agent.send("POST", &uri(&task.id), json!({})),
                403,
                "authorization_denied",
            );
            assert_eq!(payload["operation"], "auto_drain.auto_fix");

            assert_eq!(
                fixture
                    .runtime
                    .list_job_runs(JobRunListParams::default())
                    .unwrap(),
                before,
                "a refused Fix PR must not persist a run"
            );
            assert_eq!(
                fixture.runtime.get_task(&task.id).unwrap().status,
                TaskStatus::Review
            );
        },
    );
}
