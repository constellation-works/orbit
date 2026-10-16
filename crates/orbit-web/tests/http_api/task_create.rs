//! Public proposed-only capture, retry and live authority guarantees.
use super::support::{Fixture, Server, isolated, json_ok};
use orbit_core::application::task::TaskUpdateParams;
use orbit_types::workspace::WorkspaceCheckoutRole;
use serde_json::{Value, json};

const ACTION: &str = "/api/tasks/create?workspace=ws_http_fixture";

fn proposal(server: &Server) -> Value {
    let workspaces = json_ok(server.get("/api/workspaces"));
    json!({
        "request_id": "proposal-recovery-1", "binding": workspaces[0]["creation_binding"],
        "title": "Capture a proposed task", "description": "Human-authored description",
        "complexity": "low", "acceptance_criteria": ["One task survives retries"],
    })
}

fn edit_registry(
    fixture: &Fixture,
    edit: impl FnOnce(&mut orbit_types::workspace::WorkspaceRegistry),
) {
    let path = orbit_registry::workspace_registry::registry_path_for(&fixture.global);
    let mut registry = orbit_registry::workspace_registry::load_registry_from(&path).unwrap();
    edit(&mut registry);
    orbit_registry::workspace_registry::save_registry_to(&registry, &path).unwrap();
}

#[test]
fn proposed_creation_is_atomic_and_receipt_survives_mutation_and_restart() {
    isolated(
        "task_create::proposed_creation_is_atomic_and_receipt_survives_mutation_and_restart",
        || {
            let fixture = Fixture::new();
            let server = fixture.server(true);
            let body = proposal(&server);
            let receipts = std::thread::scope(|scope| {
                let requests: Vec<_> = (0..2)
                    .map(|_| scope.spawn(|| json_ok(server.send("POST", ACTION, body.clone()))))
                    .collect();
                requests
                    .into_iter()
                    .map(|request| request.join().unwrap())
                    .collect::<Vec<_>>()
            });
            assert_eq!(receipts[0], receipts[1]);
            let receipt = &receipts[0];
            assert_eq!(receipt.as_object().unwrap().len(), 3);
            assert_eq!(receipt["workspace_id"], "ws_http_fixture");
            assert_eq!(receipt["request_id"], body["request_id"]);
            let mut canonical = body.clone();
            canonical["priority"] = json!("medium");
            canonical["task_type"] = json!("chore");
            canonical["title"] = json!("  Capture a proposed task  ");
            assert_eq!(json_ok(server.send("POST", ACTION, canonical)), *receipt);
            let id = receipt["id"].as_str().unwrap();
            let task = json_ok(server.get(&format!("/api/tasks/{id}?workspace=ws_http_fixture")));
            assert_eq!(task["status"], "proposed");
            assert_eq!(task["created_by"], "human");
            assert_eq!(task["complexity"], "low");
            assert_eq!(task["acceptance_criteria"], body["acceptance_criteria"]);
            fixture
                .runtime
                .update_task_as_human(
                    id,
                    TaskUpdateParams {
                        title: Some("Changed after original creation".into()),
                        ..Default::default()
                    },
                    "human".into(),
                )
                .unwrap();
            let mut changed = body.clone();
            changed["title"] = json!("Another task");
            assert!(!server.send("POST", ACTION, changed).status().is_success());
            assert_eq!(json_ok(server.send("POST", ACTION, body.clone())), *receipt);
            // A damaged linked run makes mutable task-detail projection fail.
            // Creation lookup needs neither that projection nor the run store.
            fixture.seed_run(
                "proposal-projection",
                "fixture",
                orbit_core::JobRunState::Success,
            );
            fixture
                .runtime
                .force_update_task_with_identity(
                    id,
                    TaskUpdateParams {
                        status: Some(orbit_core::TaskStatus::Review),
                        job_run_id: Some(Some("proposal-projection".into())),
                        ..Default::default()
                    },
                    None,
                    None,
                )
                .unwrap();
            let run_store = fixture.runtime.sqlite_store().unwrap();
            let connection = run_store.connection();
            connection.lock().unwrap().execute(
                "UPDATE job_runs SET state = 'invalid-projection' WHERE run_id = 'proposal-projection'", [],
            ).unwrap();
            assert_eq!(
                server
                    .get(&format!("/api/tasks/{id}?workspace=ws_http_fixture"))
                    .status()
                    .as_u16(),
                500
            );
            assert_eq!(json_ok(server.send("POST", ACTION, body.clone())), *receipt);
            connection
                .lock()
                .unwrap()
                .execute(
                    "UPDATE job_runs SET state = 'success' WHERE run_id = 'proposal-projection'",
                    [],
                )
                .unwrap();
            // Discarding the first response models a committed write with no client
            // receipt; a new server must recover it from durable action keys.
            drop(server);
            let restarted = fixture.server(true);
            assert_eq!(
                proposal(&restarted)["binding"],
                body["binding"],
                "binding survives restart"
            );
            assert_eq!(
                json_ok(restarted.send("POST", ACTION, body.clone())),
                *receipt
            );
            assert_eq!(fixture.runtime.list_tasks().unwrap().len(), 1);
        },
    );
}

#[test]
fn proposed_creation_refuses_invalid_selection_authority_and_input_without_writes() {
    isolated(
        "task_create::proposed_creation_refuses_invalid_selection_authority_and_input_without_writes",
        || {
            let fixture = Fixture::new();
            let server = fixture.server(true);
            let body = proposal(&server);
            for path in [
                "/api/tasks/create",
                "/api/tasks/create?workspace=",
                "/api/tasks/create?workspace=unknown",
                "/api/tasks/create?workspace=ws_http_fixture&workspace=ws_http_fixture",
            ] {
                assert!(
                    !server
                        .send("POST", path, body.clone())
                        .status()
                        .is_success(),
                    "{path}"
                );
            }
            for (field, value) in [
                ("title", json!("  ")),
                ("acceptance_criteria", json!([])),
                ("acceptance_criteria", json!([" "])),
                ("complexity", json!("unassessed")),
                ("complexity", json!("made-up")),
                ("priority", json!("made-up")),
                ("request_id", json!("/unsafe")),
                ("request_id", json!("x".repeat(129))),
                ("status", json!("backlog")),
                ("force", json!(true)),
                ("system_created", json!(true)),
                ("model", json!("codex")),
                ("authority", json!("operator")),
                ("crew", json!("other")),
            ] {
                let mut invalid = body.clone();
                invalid[field] = value;
                assert!(
                    !server.send("POST", ACTION, invalid).status().is_success(),
                    "{field}"
                );
            }
            for (header, value) in [
                ("origin", "http://evil.example"),
                ("host", "evil.example"),
                ("sec-fetch-site", "cross-site"),
            ] {
                let request = server.request("POST", ACTION);
                let request = if header == "origin" {
                    request
                } else {
                    request.header("origin", &server.origin)
                };
                let response = request.header(header, value).json(&body).send().unwrap();
                assert_eq!(response.status().as_u16(), 403, "{header}");
            }
            // Regression: duplicate Origin headers previously checked only the
            // first value, admitting a legacy write with a conflicting origin.
            let duplicate_origin = server.request("POST", "/api/tasks?workspace=ws_http_fixture")
                .header("origin", &server.origin).header("origin", "http://evil.example")
                .json(&json!({"title": "Forbidden duplicate origin", "description": "Must not be stored", "complexity": "low"}))
                .send().unwrap();
            assert_eq!(duplicate_origin.status().as_u16(), 403);
            let malformed = server
                .request("POST", ACTION)
                .header("origin", &server.origin)
                .header("content-type", "application/json")
                .body("{")
                .send()
                .unwrap();
            assert!(!malformed.status().is_success());
            let unprivileged = fixture.server(false);
            assert_eq!(
                unprivileged
                    .send("POST", ACTION, body.clone())
                    .status()
                    .as_u16(),
                403
            );
            edit_registry(&fixture, |registry| {
                registry.checkouts[0].repo_root = fixture.path("missing-checkout")
            });
            assert!(!server.send("POST", ACTION, body).status().is_success());
            assert!(fixture.runtime.list_tasks().unwrap().is_empty());
        },
    );
}

#[test]
fn replay_rechecks_live_permission_owner_role_binding_and_registry() {
    isolated(
        "task_create::replay_rechecks_live_permission_owner_role_binding_and_registry",
        || {
            let fixture = Fixture::new();
            let server = fixture.server(true);
            let body = proposal(&server);
            let receipt = json_ok(server.send("POST", ACTION, body.clone()));
            let assert_refused = |server: &Server| {
                let response = server.send("POST", ACTION, body.clone());
                assert!(!response.status().is_success());
                let refused: Value = response.json().unwrap();
                assert!(
                    refused.get("id").is_none(),
                    "historical receipts cannot leak: {refused}"
                );
            };
            let cross_origin = server
                .request("POST", ACTION)
                .header("origin", "http://evil.example")
                .json(&body)
                .send()
                .unwrap();
            assert_eq!(cross_origin.status().as_u16(), 403);
            assert!(cross_origin.json::<Value>().unwrap().get("id").is_none());
            let denied = fixture.server(false);
            assert_refused(&denied);
            let config_path = fixture
                .global
                .join(orbit_registry::machine_identity::CONFIG_TOML_FILE);
            let config = std::fs::read_to_string(&config_path).unwrap();
            let changed_identity = config.replace(&fixture.machine_id(), "hm_bbbbbbbbbbbbbbbb");
            assert_ne!(changed_identity, config);
            std::fs::write(&config_path, changed_identity).unwrap();
            assert_refused(&server);
            std::fs::write(&config_path, config).unwrap();
            assert_eq!(json_ok(server.send("POST", ACTION, body.clone())), receipt);
            let registry_path =
                orbit_registry::workspace_registry::registry_path_for(&fixture.global);
            let original = std::fs::read(&registry_path).unwrap();
            for mutation in 0..4 {
                edit_registry(&fixture, |registry| match mutation {
                    0 => {
                        registry.workspaces[0].owner_machine_id = Some("hm_other_owner".into());
                        registry.checkouts[0].role = Some(WorkspaceCheckoutRole::Replica);
                        registry.checkouts[0].owner_machine_id = Some("hm_other_owner".into());
                    }
                    1 => {
                        registry.checkouts[0].role = Some(WorkspaceCheckoutRole::Replica);
                        registry.checkouts[0].owner_machine_id = Some("hm_role_owner".into());
                        registry.workspaces[0].owner_machine_id = Some("hm_role_owner".into());
                    }
                    2 => registry.workspaces[0].base_branch = "rebound-branch".into(),
                    _ => registry.checkouts[0].repo_root = fixture.path("missing-checkout"),
                });
                assert_refused(&server);
                std::fs::write(&registry_path, &original).unwrap();
                assert_eq!(json_ok(server.send("POST", ACTION, body.clone())), receipt);
            }
            std::fs::write(&registry_path, b"not valid registry JSON").unwrap();
            assert_refused(&server);
            std::fs::write(&registry_path, &original).unwrap();
            assert_eq!(json_ok(server.send("POST", ACTION, body)), receipt);
            assert_eq!(fixture.runtime.list_tasks().unwrap().len(), 1);
        },
    );
}

#[test]
#[ignore = "requires ORBIT_PLAYWRIGHT_MODULE and ORBIT_CREATE_BROWSER_EVIDENCE_DIR"]
fn dashboard_proposal_form_retry_and_scope_recovery() {
    isolated(
        "task_create::dashboard_proposal_form_retry_and_scope_recovery",
        || {
            let fixture = Fixture::new();
            let server = fixture.server(true);
            let output = std::process::Command::new("node")
                .arg(
                    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join("tests/http_api/dashboard_task_create_browser.mjs"),
                )
                .arg(
                    std::env::var_os("ORBIT_PLAYWRIGHT_MODULE")
                        .expect("prepared Playwright module"),
                )
                .arg(
                    std::env::var_os("ORBIT_CREATE_BROWSER_EVIDENCE_DIR")
                        .expect("browser evidence directory"),
                )
                .arg(&server.origin)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        },
    );
}
