//! The embedded server retains its caller's workspace and storage identity.

use std::fs;

use orbit_core::application::task::TaskAddParams;
use serde_json::json;

use super::support::{Fixture, Server, isolated, json_ok};

fn assert_task(server: &Server, id: &str, phase: &str) {
    let tasks = json_ok(server.get("/api/tasks?workspace=default"));
    assert!(
        tasks["items"]
            .as_array()
            .expect("task list")
            .iter()
            .any(|task| task["id"] == id),
        "supplied workspace task must survive {phase}: {tasks}"
    );
    let health = json_ok(server.get("/healthz?detailed=true"));
    assert_eq!(health["workspaces_open"], 1, "open runtime after {phase}");
}

#[test]
fn embedded_serve_preserves_workspace_after_config_changes() {
    isolated(
        "embedded::embedded_serve_preserves_workspace_after_config_changes",
        || {
            let fixture = Fixture::new();
            let task = fixture
                .runtime
                .add_task(TaskAddParams {
                    title: "Embedded workspace task".into(),
                    description: "Must remain in the supplied storage partition".into(),
                    ..Default::default()
                })
                .unwrap();
            let server = fixture
                .server_with_envs(true, &[("ORBIT_HTTP_EMBEDDED", std::ffi::OsStr::new("1"))]);
            assert_task(&server, &task.id, "startup");

            let global_path = fixture.global.join("config.toml");
            let mut global = fs::read_to_string(&global_path).unwrap();
            // Changing length makes invalidation deterministic even on coarse clocks.
            global.push_str("\n# Valid external edit while embedded server is running\n");
            fs::write(&global_path, global).unwrap();
            assert_task(&server, &task.id, "external global config edit");

            for (scope, expected_path) in [
                ("global", global_path),
                ("workspace", fixture.work.join("config.toml")),
            ] {
                let value = format!("embedded-{scope}-branch");
                let write = json_ok(server.send(
                    "PUT",
                    "/api/config/keys/workflow.base_branch?workspace=default",
                    json!({"value": value, "scope": scope, "init": "fresh"}),
                ));
                assert_eq!(write["path"], expected_path.to_str().unwrap());
                assert_eq!(write["new_value"], value);
                assert_task(
                    &server,
                    &task.id,
                    &format!("dashboard {scope} config write"),
                );
                // ConfigFileWs uses a separate resolver; it must retain the same roots.
                let file = json_ok(
                    server.get(&format!("/api/config/file?scope={scope}&workspace=default")),
                );
                let displayed_path = std::path::Path::new("~")
                    .join(expected_path.strip_prefix(fixture.path("")).unwrap());
                assert_eq!(file["file"]["path"], displayed_path.to_str().unwrap());
            }

            let updated = json_ok(server.send(
                "PATCH",
                &format!("/api/tasks/{}?workspace=default", task.id),
                json!({"title": "Updated through embedded server"}),
            ));
            assert_eq!(updated["id"], task.id);
            assert_eq!(
                fixture.runtime.get_task(&task.id).unwrap().title,
                "Updated through embedded server",
                "HTTP writes must use the supplied runtime's storage partition"
            );
            assert!(
                !fixture.path("state").exists(),
                "embedded resolution must not initialize cwd-relative state"
            );
        },
    );
}
