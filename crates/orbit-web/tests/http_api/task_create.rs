//! Public proposed-only capture, retry and live authority guarantees.
use super::support::{Fixture, Server, isolated, json_ok};
use orbit_core::application::task::TaskUpdateParams;
use orbit_types::workspace::WorkspaceCheckoutRole;
use serde_json::{Value, json};

const ACTION: &str = "/api/tasks/create?workspace=ws_http_fixture";

#[test]
fn workspace_listing_omits_source_remote_even_for_legacy_credentials() {
    isolated(
        "task_create::workspace_listing_omits_source_remote_even_for_legacy_credentials",
        || {
            let fixture = Fixture::new();
            edit_registry(&fixture, |registry| {
                registry.workspaces[0].git_remote =
                    Some("https://operator:fixture-secret@github.com/example/orbit.git".into());
            });
            let server = fixture.server(true);
            let workspaces = json_ok(server.get("/api/workspaces"));
            let rows = workspaces.as_array().unwrap();
            assert_eq!(rows.len(), 1);
            assert!(rows[0].get("git_remote").is_none());
            assert!(!workspaces.to_string().contains("fixture-secret"));
        },
    );
}

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

fn task_registry(fixture: &Fixture) -> orbit_store::maintenance::task_registry::TaskRegistryStore {
    use orbit_store::maintenance::task_registry::{TaskRegistryStore, task_registry_path};
    TaskRegistryStore::open(&task_registry_path(&fixture.global)).unwrap()
}

#[test]
fn receipt_recovery_does_not_read_or_repair_damaged_sidecars() {
    isolated(
        "task_create::receipt_recovery_does_not_read_or_repair_damaged_sidecars",
        || {
            let fixture = Fixture::new();
            let server = fixture.server(true);
            let body = proposal(&server);
            let receipt = json_ok(server.send("POST", ACTION, body.clone()));
            assert_eq!(receipt.as_object().unwrap().len(), 3);
            let id = receipt["id"].as_str().unwrap();
            let binding = task_registry(&fixture)
                .find_task_binding(id)
                .unwrap()
                .unwrap();
            let comments = binding.canonical_path.join("comments.jsonl");
            let original = std::fs::read(&comments).unwrap();
            let damaged = b"not a JSON comment\n{}\n";
            std::fs::write(&comments, damaged).unwrap();
            let detail = format!("/api/tasks/{id}?workspace=ws_http_fixture");
            assert_eq!(server.get(&detail).status().as_u16(), 500);
            // Restart rules out recovery from an in-memory successful response.
            drop(server);
            let server = fixture.server(true);
            std::thread::scope(|scope| {
                let requests: Vec<_> = (0..2)
                    .map(|_| scope.spawn(|| json_ok(server.send("POST", ACTION, body.clone()))))
                    .collect();
                for request in requests {
                    assert_eq!(request.join().unwrap(), receipt);
                }
            });
            let mut changed = body.clone();
            changed["title"] = json!("Changed input with damaged comments");
            let response = server.send("POST", ACTION, changed);
            assert_eq!(response.status().as_u16(), 400);
            assert!(response.json::<Value>().unwrap().get("id").is_none());
            assert_eq!(server.get(&detail).status().as_u16(), 500);
            assert_eq!(std::fs::read(&comments).unwrap(), damaged);
            std::fs::write(&comments, original).unwrap();
            fixture
                .runtime
                .update_task_as_human(
                    id,
                    TaskUpdateParams {
                        upsert_artifacts: vec![orbit_types::task::TaskArtifact {
                            path: "receipt-evidence.txt".into(),
                            media_type: "text/plain".into(),
                            content: b"original evidence".to_vec(),
                            created_by: None,
                        }],
                        ..Default::default()
                    },
                    "human".into(),
                )
                .unwrap();
            let manifest: orbit_types::task::ArtifactManifestV2 = serde_yaml::from_str(
                &std::fs::read_to_string(binding.canonical_path.join("artifacts/manifest.yaml"))
                    .unwrap(),
            )
            .unwrap();
            let blob = binding
                .canonical_path
                .join("artifacts")
                .join(&manifest.files[0].blob);
            let artifact_path =
                format!("/api/tasks/{id}/artifacts/receipt-evidence.txt?workspace=ws_http_fixture");
            let response = server.get(&artifact_path);
            assert!(response.status().is_success());
            assert_eq!(response.text().unwrap(), "original evidence");
            std::fs::rename(&blob, binding.canonical_path.join("saved-evidence.txt")).unwrap();
            assert_eq!(json_ok(server.send("POST", ACTION, body)), receipt);
            assert_eq!(server.get(&detail).status().as_u16(), 500);
            assert!(!server.get(&artifact_path).status().is_success());
            assert!(
                !blob.exists(),
                "replay must not repair or conceal missing payloads"
            );
            assert_eq!(
                task_registry(&fixture)
                    .tasks_for_workspace("ws_http_fixture")
                    .unwrap()
                    .len(),
                1
            );
        },
    );
}

#[test]
fn receipt_recovery_requires_committed_matching_identity_and_reuses_reservation() {
    isolated(
        "task_create::receipt_recovery_requires_committed_matching_identity_and_reuses_reservation",
        || {
            let fixture = Fixture::new();
            let server = fixture.server(true);
            let mut body = proposal(&server);
            let template = json_ok(server.send("POST", ACTION, body.clone()));
            let registry = task_registry(&fixture);
            let template_binding = registry
                .find_task_binding(template["id"].as_str().unwrap())
                .unwrap()
                .unwrap();
            let mut envelope: orbit_types::task::TaskEnvelopeV2 = serde_yaml::from_str(
                &std::fs::read_to_string(template_binding.canonical_path.join("task.yaml"))
                    .unwrap(),
            )
            .unwrap();
            body["request_id"] = json!("interrupted-proposal");
            let mut input: orbit_core::application::task::ProposedTaskInput =
                serde_json::from_value(body.clone()).unwrap();
            input.task_type = Some(orbit_core::TaskType::Chore);
            let digest =
                orbit_common::security::release::sha256_hex(&serde_json::to_vec(&input).unwrap());
            let id = registry.allocate_task_id("ws_http_fixture").unwrap();
            let connection = rusqlite::Connection::open(
                orbit_store::maintenance::task_registry::task_registry_path(&fixture.global),
            )
            .unwrap();
            connection
                .execute(
                    "INSERT INTO task_action_keys VALUES (?1, ?2, ?3, ?4)",
                    rusqlite::params![
                        "ws_http_fixture",
                        "dashboard-create:interrupted-proposal",
                        id,
                        digest
                    ],
                )
                .unwrap();
            let path = registry
                .canonical_task_bundle_path("ws_http_fixture", &id)
                .unwrap();
            std::fs::create_dir_all(&path).unwrap();
            envelope.id = id.clone();
            std::fs::write(
                path.join("task.yaml"),
                serde_yaml::to_string(&envelope).unwrap(),
            )
            .unwrap();
            let assert_refused = || {
                let response = server.send("POST", ACTION, body.clone());
                assert!(!response.status().is_success());
                assert!(response.json::<Value>().unwrap().get("id").is_none());
                let reserved: String = connection
                    .query_row(
                        "SELECT task_id FROM task_action_keys WHERE action_key = ?1",
                        ["dashboard-create:interrupted-proposal"],
                        |row| row.get(0),
                    )
                    .unwrap();
                assert_eq!(reserved, id, "a refused replay retains its allocation");
            };
            // A valid envelope alone is not a committed bundle.
            assert_refused();
            assert!(registry.find_task_binding(&id).unwrap().is_none());
            // Even a registration cannot turn a missing or mismatched envelope
            // into a historical receipt.
            registry
                .register_task_bundle(&id, "ws_http_fixture", &path)
                .unwrap();
            std::fs::rename(path.join("task.yaml"), path.join("saved-envelope.yaml")).unwrap();
            assert_refused();
            envelope.id = template["id"].as_str().unwrap().into();
            std::fs::write(
                path.join("task.yaml"),
                serde_yaml::to_string(&envelope).unwrap(),
            )
            .unwrap();
            assert_refused();
            // A contradictory owner binding must fail closed as well.
            registry
                .register_workspace(
                    orbit_store::maintenance::task_registry::RegisterWorkspaceParams {
                        partition_id: "ws_other_owner".into(),
                        slug: "other".into(),
                        repo_fingerprint: None,
                    },
                )
                .unwrap();
            connection.execute("UPDATE task_bundle_bindings SET workspace_id = 'ws_other_owner' WHERE task_id = ?1", [&id]).unwrap();
            envelope.id = id.clone();
            std::fs::write(
                path.join("task.yaml"),
                serde_yaml::to_string(&envelope).unwrap(),
            )
            .unwrap();
            assert_refused();
            connection.execute("UPDATE task_bundle_bindings SET workspace_id = 'ws_http_fixture' WHERE task_id = ?1", [&id]).unwrap();
            connection
                .execute(
                    "UPDATE task_bundle_bindings SET canonical_path = ?1 WHERE task_id = ?2",
                    rusqlite::params![template_binding.canonical_path.to_str().unwrap(), id],
                )
                .unwrap();
            assert_refused();
            connection
                .execute(
                    "UPDATE task_bundle_bindings SET canonical_path = ?1 WHERE task_id = ?2",
                    rusqlite::params![path.to_str().unwrap(), id],
                )
                .unwrap();
            registry
                .unregister_task_bundle(&id, "ws_http_fixture")
                .unwrap();
            // Retain the interrupted bytes outside the canonical namespace. The
            // same request can now publish, using its original reserved ID.
            std::fs::rename(&path, fixture.path("interrupted-bundle")).unwrap();
            let receipt = json_ok(server.send("POST", ACTION, body.clone()));
            assert_eq!(receipt["id"], id);
            // A complete publication interrupted before registration is
            // reconciled by the existing recovery path, without a new task.
            registry
                .unregister_task_bundle(&id, "ws_http_fixture")
                .unwrap();
            assert_eq!(json_ok(server.send("POST", ACTION, body)), receipt);
            assert_eq!(
                json_ok(server.get(&format!("/api/tasks/{id}?workspace=ws_http_fixture")))["id"],
                id
            );
            assert_eq!(
                registry
                    .tasks_for_workspace("ws_http_fixture")
                    .unwrap()
                    .len(),
                2
            );
        },
    );
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
            // A historical receipt remains behind live authorization even when
            // normal task materialization would fail on mutable corruption.
            let binding = task_registry(&fixture)
                .find_task_binding(receipt["id"].as_str().unwrap())
                .unwrap()
                .unwrap();
            let comments = binding.canonical_path.join("comments.jsonl");
            let original_comments = std::fs::read(&comments).unwrap();
            std::fs::write(&comments, b"invalid comment JSON\n{}\n").unwrap();
            let assert_refused = |server: &Server| {
                let response = server.send("POST", ACTION, body.clone());
                assert!(
                    response.status().is_client_error(),
                    "authority refusal must precede damaged-bundle reads: {}",
                    response.status()
                );
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
            std::fs::write(&comments, original_comments).unwrap();
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
