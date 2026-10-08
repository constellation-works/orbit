use std::collections::BTreeSet;

use orbit_core::JobRunState;
use serde_json::{Value, json};

use super::support::{Fixture, isolated};

#[test]
fn task_mutations_redact_prose_before_persistence() {
    isolated(
        "guards::task_mutations_redact_prose_before_persistence",
        || {
            let fixture = Fixture::new();
            let server = fixture.server(true);
            let token = format!("ghp_{}", "b".repeat(36));
            let text = format!("diagnostic GITHUB_TOKEN={token}");
            let safe = "diagnostic GITHUB_TOKEN=[REDACTED_SECRET]";
            let task = super::support::json_ok(server.send(
                "POST",
                "/api/tasks?workspace=ws_http_fixture",
                json!({"title":"Redaction fixture", "description":"HTTP mutation test",
                "acceptance_criteria":["Persist scrubbed prose"], "complexity":"low"}),
            ));
            let id = task["id"].as_str().unwrap();
            let uri = format!("/api/tasks/{id}?workspace=ws_http_fixture");
            let updated = super::support::json_ok(server.send(
                "PATCH",
                &uri,
                json!({"title":text, "description":text, "plan":text, "execution_summary":text,
                "acceptance_criteria":[text,"ordinary criterion"], "comment":text}),
            ));
            for field in ["title", "description", "plan", "execution_summary"] {
                assert_eq!(updated[field], safe, "updated {field}");
            }
            assert_eq!(
                updated["acceptance_criteria"],
                json!([safe, "ordinary criterion"])
            );
            super::support::json_ok(server.send(
                "POST",
                &format!("/api/tasks/{id}/comments?workspace=ws_http_fixture"),
                json!({"message":text}),
            ));
            let approved = super::support::json_ok(server.send(
                "POST",
                &format!("/api/tasks/{id}/approve?workspace=ws_http_fixture"),
                json!({"note":text,"comment":text}),
            ));
            assert_eq!(approved["status"], "backlog");
            assert!(
                approved["history"].as_array().unwrap().iter().any(|event| {
                    event["event"] == "proposal_approved" && event["note"] == safe
                })
            );
            let rejected = super::support::json_ok(server.send(
                "POST",
                &format!("/api/tasks/{id}/reject?workspace=ws_http_fixture"),
                json!({"note":text,"comment":text}),
            ));
            assert_eq!(rejected["status"], "rejected");
            assert!(
                rejected["history"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|event| { event["event"] == "backlog_rejected" && event["note"] == safe })
            );
            let persisted = super::support::json_ok(server.get(&uri));
            let comments = persisted["comments"].as_array().unwrap();
            assert_eq!(comments.len(), 4);
            assert!(comments.iter().all(|comment| comment["message"] == safe));
            // The selected logical workspace may use a different persisted
            // task partition. Locate its canonical bundle by the created ID.
            let bundle = std::fs::read_dir(fixture.global.join("tasks/workspaces"))
                .unwrap()
                .map(|entry| entry.unwrap().path().join(id))
                .find(|path| path.is_dir())
                .expect("persisted task bundle");
            for name in [
                "task.yaml",
                "description.md",
                "plan.md",
                "execution-summary.md",
                "acceptance.md",
                "comments.jsonl",
                "events.jsonl",
            ] {
                let content = std::fs::read_to_string(bundle.join(name)).unwrap();
                assert!(!content.contains(&token), "task secret leaked into {name}");
                assert!(
                    content.contains("[REDACTED_SECRET]"),
                    "scrubbed prose missing from {name}"
                );
            }
        },
    );
}

/// Ordinary writes intentionally outside the exceptional-operation registry.
/// New methods/paths default to operator-only, so a new unguarded route fails.
/// Resume and auto-drain are ordinary only when the request does not ask for completion.
const ORDINARY_WRITES: &[(&str, &str)] = &[
    ("POST", "/tasks"),
    ("PATCH", "/tasks/:id"),
    ("POST", "/tasks/:id/comments"),
    ("POST", "/tasks/:id/approve"),
    ("POST", "/tasks/:id/reject"),
    ("POST", "/tasks/:id/archive"),
    ("POST", "/frictions"),
    ("PATCH", "/frictions/:id"),
    ("POST", "/frictions/:id/resolve"),
    ("POST", "/job-runs/:id/resume"),
    ("POST", "/workflows/auto"),
    ("POST", "/workflows/ship"),
    ("POST", "/runs/:id/cancel"),
    ("POST", "/metrics/invocations"),
];

/// Discover paths from registrations, then ask the real HTTP router for methods.
/// This source input is only discovery for a security guard, never a text assertion.
/// A nonliteral registration fails closed instead of silently dropping coverage.
fn registered_paths() -> impl Iterator<Item = &'static str> {
    include_str!("../../src/api/routes.rs")
        .split(".route(")
        .skip(1)
        .map(|registration| {
            registration
                .trim_start()
                .strip_prefix('"')
                .and_then(|literal| literal.split('"').next())
                .expect(
                    "router-wide origin/operator safety coverage requires discoverable route paths",
                )
        })
}

fn body(path: &str) -> Value {
    match path {
        "/runs/:id/replay" => json!({}),
        "/config/keys/:key" => json!({"value":"agent-main","init":"fresh"}),
        "/config/crews/:name" => json!({"fields":{}}),
        "/routines/clock" => json!({
            "action":"disable", "machine_name":"fixture", "expected_enabled":true,
            "expected_cadence_seconds":300,
        }),
        "/routines/toggle" => json!({
            "name":"missing", "source":"fixture", "target":"job:missing",
            "machine_name":"fixture", "expected_enabled":true,"enabled":false,
        }),
        "/auto-tasks/toggle" => json!({"name":"missing","expected_enabled":true,"enabled":false}),
        "/auto-tasks/mint" => json!({"name":"missing","acknowledge_unconditional":true}),
        "/workflows/auto" => json!({"for_duration":"1m","complete":false}),
        _ => json!({
            "title":"guard fixture", "description":"HTTP guard check", "scope":"host",
            "expected_candidate_commit":"a", "expected_base_commit":"b",
            "expected_phase":"claimed", "request_id":"guard-fixture", "status":"blocked",
            "reason":"HTTP guard check", "comment":"HTTP guard check",
            "task_ids":["missing"], "mode":"local",
        }),
    }
}

#[test]
fn every_router_mutation_enforces_origin_and_operator_policy() {
    isolated(
        "guards::every_router_mutation_enforces_origin_and_operator_policy",
        || {
            let fixture = Fixture::new();
            fixture.job("resume_guard_fixture");
            for completion in ["review", "done"] {
                let mut source = fixture.seed_run(
                    &format!("jrun-resume-{completion}"),
                    "resume_guard_fixture",
                    JobRunState::Failed,
                );
                source.input = Some(json!({"completion":completion}));
                fixture.save_run(&source);
            }
            let operator = fixture.server(true);
            let agent = fixture.server(false);
            let mut mutations = BTreeSet::new();
            for (path, id) in registered_paths().flat_map(|path| {
                // Resume inherits completion from saved input, not the HTTP body.
                let ids: &[&str] = if path == "/job-runs/:id/resume" {
                    &["jrun-resume-review", "jrun-resume-done"]
                } else {
                    &["missing"]
                };
                ids.iter().map(move |&id| (path, id))
            }) {
                let concrete = path
                    .replace(":id", id)
                    .replace(":key", "workflow.base_branch")
                    .replace(":name", "missing")
                    .replace(":namespace", "missing")
                    .replace(":panel", "missing")
                    .replace(":kind", "routine")
                    .replace(":batch", "missing")
                    .replace("*path", "missing");
                let uri = format!("/api{concrete}?workspace=ws_http_fixture");
                let discovery = operator.request("OPTIONS", &uri).send().unwrap();
                assert_eq!(discovery.status().as_u16(), 405, "route discovery: {path}");
                let allowed = discovery
                    .headers()
                    .get("allow")
                    .expect("router's HTTP Allow header")
                    .to_str()
                    .unwrap()
                    .to_string();
                for method in allowed
                    .split(',')
                    .map(str::trim)
                    .filter(|method| matches!(*method, "POST" | "PUT" | "PATCH" | "DELETE"))
                {
                    mutations.insert((method.to_string(), path.to_string()));
                    // Cover CSRF, DNS rebinding, missing/malformed host, and authority mismatch.
                    for (host, origin) in [
                        (None, None),
                        (None, Some("https://attacker.example")),
                        (Some("attacker.example"), Some(operator.origin.as_str())),
                        (Some(""), Some(operator.origin.as_str())),
                        (None, Some("http://localhost:1")),
                        (None, Some("null")),
                    ] {
                        let mut request = operator.request(method, &uri).json(&body(path));
                        if let Some(host) = host {
                            request = request.header("host", host);
                        }
                        if let Some(origin) = origin {
                            request = request.header("origin", origin);
                        }
                        let response = request.send().unwrap();
                        assert_eq!(
                            response.status().as_u16(),
                            403,
                            "origin guard must protect {method} {path}, host={host:?}, origin={origin:?}"
                        );
                        assert_eq!(response.headers()["x-content-type-options"], "nosniff");
                        let payload: Value = response.json().unwrap();
                        assert!(payload["error"].is_string(), "{payload}");
                    }

                    let bodies = if path == "/workflows/auto" {
                        vec![
                            json!({"for_duration":"1m","complete":false}),
                            json!({"for_duration":"1m","complete":true}),
                            json!({"for_duration":"1m","approve_proposed":true}),
                        ]
                    } else {
                        vec![body(path)]
                    };
                    for probe_body in bodies {
                        let response = agent.send(method, &uri, probe_body.clone());
                        let status = response.status().as_u16();
                        let payload: Value = response.json().unwrap();
                        let completion_resume =
                            path == "/job-runs/:id/resume" && id == "jrun-resume-done";
                        let completion_auto =
                            path == "/workflows/auto" && probe_body["complete"] == true;
                        let approve_auto =
                            path == "/workflows/auto" && probe_body["approve_proposed"] == true;
                        let operator_only = completion_resume || completion_auto || approve_auto;
                        if ORDINARY_WRITES.contains(&(method, path)) && !operator_only {
                            assert_ne!(
                                payload["code"], "authorization_denied",
                                "ordinary write must preserve agent admission: {method} {path}: {status} {payload}"
                            );
                        } else {
                            assert_eq!(
                                status, 403,
                                "operator guard must protect {method} {path}: {payload}"
                            );
                            assert_eq!(
                                payload["code"], "authorization_denied",
                                "{method} {path}: {payload}"
                            );
                            if completion_resume || completion_auto {
                                assert_eq!(payload["operation"], "auto_drain.complete");
                            }
                            if approve_auto {
                                assert_eq!(payload["operation"], "auto_drain.approve_proposed");
                            }
                        }
                    }
                }
            }
            for &(method, path) in ORDINARY_WRITES {
                assert!(
                    mutations.contains(&(method.into(), path.into())),
                    "stale authorization exception has no HTTP route: {method} {path}"
                );
            }
            assert!(
                !mutations.is_empty(),
                "router-wide mutation security table must execute HTTP checks"
            );
        },
    );
}

#[test]
fn effective_config_lists_every_crew_setting_and_enabled_auto_task() {
    isolated(
        "guards::effective_config_lists_every_crew_setting_and_enabled_auto_task",
        || {
            let fixture = Fixture::new();
            let global_path = fixture.global.join("config.toml");
            let mut global = std::fs::read_to_string(&global_path).unwrap();
            global.push_str(concat!(
                "\n[workflow]\ndefault_crew = \"opus\"\nsystem_crew = \"sol\"\n",
                "final_recovery_crews = [\"opus:20\", \"sol:100\"]\n",
                "low_complexity_crews = [\"haiku\", \" haiku \"]\n",
                "medium_complexity_crews = [\" sol : 2 \"]\n",
                "hard_complexity_crews = [\"opus\"]\n",
                "xhard_complexity_crews = [\"opus:3\"]\n",
                "\n[operation]\nreview_crew = \"opus\"\n",
                "\n[crews.opus]\nprovider = \"claude\"\nmodel = \"opus\"\n",
                "\n[crews.sol]\nprovider = \"codex\"\nmodel = \"fixture-sol\"\n",
                "\n[crews.haiku]\nprovider = \"claude\"\nmodel = \"haiku\"\n",
                "\n[crews.grok]\nprovider = \"grok\"\nmodel = \"fixture-grok\"\n",
            ));
            std::fs::write(global_path, global).unwrap();
            std::fs::write(
                fixture.work.join("config.toml"),
                "[operation]\nreview_crew = \"haiku\"\n",
            )
            .unwrap();
            let definitions = fixture.work.join("auto_tasks");
            std::fs::create_dir_all(&definitions).unwrap();
            for (name, enabled) in [("skill-validation", true), ("disabled-validation", false)] {
                std::fs::write(
                    definitions.join(format!("{name}.yaml")),
                    format!(
                        "schemaVersion: 1\nname: {name}\nenabled: {enabled}\nschedule:\n  every_minutes: 60\ntemplate:\n  title: Validate fixture\n  crew: ' grok '\n"
                    ),
                )
                .unwrap();
            }
            let server = fixture.server(false);
            let view = super::support::json_ok(
                server.get("/api/config/effective?workspace=ws_http_fixture"),
            );
            let crews = view["crews"].as_array().unwrap();
            for (name, expected) in [
                (
                    "haiku",
                    json!(["operation.review_crew", "workflow.low_complexity_crews"]),
                ),
                (
                    "opus",
                    json!([
                        "workflow.default_crew",
                        "workflow.final_recovery_crews",
                        "workflow.hard_complexity_crews",
                        "workflow.xhard_complexity_crews"
                    ]),
                ),
                (
                    "sol",
                    json!([
                        "workflow.final_recovery_crews",
                        "workflow.medium_complexity_crews",
                        "workflow.system_crew"
                    ]),
                ),
                ("grok", json!(["auto-task skill-validation"])),
            ] {
                let crew = crews.iter().find(|crew| crew["name"] == name).unwrap();
                assert_eq!(
                    crew["referenced_by"], expected,
                    "effective references for {name}"
                );
            }
        },
    );
}

#[test]
fn workspace_file_views_and_writes_admit_global_crews() {
    isolated(
        "guards::workspace_file_views_and_writes_admit_global_crews",
        || {
            let fixture = Fixture::new();
            let global_path = fixture.global.join("config.toml");
            let mut global = std::fs::read_to_string(&global_path).unwrap();
            global.push_str(concat!(
                "\n[workflow]\ndefault_crew = \"x\"\nbase_branch = \"global-branch\"\n",
                "\n[crews.x]\nprovider = \"codex\"\nmodel = \"global-model\"\n",
            ));
            std::fs::write(&global_path, global).unwrap();
            let path = fixture.work.join("config.toml");
            let workspace = "[workflow]\nlow_complexity_crews = [\"x\"]\n";
            std::fs::write(&path, workspace).unwrap();
            let server = fixture.server(true);
            let url = "/api/config/file?scope=workspace&workspace=ws_http_fixture";
            let shown = super::support::json_ok(server.get(url));
            let displayed_path = shown["file"]["path"].as_str().unwrap();
            assert!(displayed_path.ends_with("/repo/.orbit/config.toml"));
            let rows = shown["sections"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|section| section["keys"].as_array().unwrap())
                .collect::<Vec<_>>();
            let pool = rows
                .iter()
                .find(|row| row["key"] == "workflow.low_complexity_crews")
                .unwrap();
            assert_eq!(pool["value"], json!(["x"]));
            assert_eq!(pool["source"]["layer"], "workspace");
            let branch = rows
                .iter()
                .find(|row| row["key"] == "workflow.base_branch")
                .unwrap();
            assert_eq!(branch["state"], "default");
            assert_ne!(
                branch["value"], "global-branch",
                "file values remain scoped"
            );
            let default_crew = rows
                .iter()
                .find(|row| row["key"] == "workflow.default_crew")
                .unwrap();
            assert_ne!(
                default_crew["value"], "x",
                "global default selection is not a scoped setting"
            );

            std::fs::write(
                &path,
                format!("{workspace}\n[crews.x]\nmodel = \"workspace-model\"\n"),
            )
            .unwrap();
            super::support::json_ok(server.get(url));
            let written = super::support::json_ok(server.send(
                "PUT",
                "/api/config/keys/workflow.base_branch?workspace=ws_http_fixture",
                json!({"value":"workspace-branch", "scope":"workspace"}),
            ));
            assert_eq!(written["new_value"], "workspace-branch");
            // Read through the file endpoint again, proving the accepted write
            // leaves a usable scoped view rather than merely returning success.
            let after = super::support::json_ok(server.get(url));
            assert!(
                after["sections"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .flat_map(|section| section["keys"].as_array().unwrap())
                    .any(|row| row["key"] == "workflow.base_branch"
                        && row["value"] == "workspace-branch")
            );

            std::fs::write(&path, "[workflow]\nlow_complexity_crews = [\"missing\"]\n").unwrap();
            let failed = server.get(url);
            assert_eq!(failed.status().as_u16(), 400);
            let error = failed.json::<Value>().unwrap();
            let reason = error["error"].as_str().unwrap();
            assert!(reason.contains("workflow.low_complexity_crews"), "{error}");
            assert!(reason.contains("missing"), "{error}");
            assert!(
                reason.contains(displayed_path),
                "file errors retain the path: {error}"
            );
        },
    );
}

#[test]
fn config_key_admission_refuses_invalid_writes_and_preserves_types() {
    isolated(
        "guards::config_key_admission_refuses_invalid_writes_and_preserves_types",
        || {
            let fixture = Fixture::new();
            let config = fixture.work.join("config.toml");
            std::fs::write(&config, "# fixture\n").unwrap();
            let server = fixture.server(true);
            for (key, value) in [
                ("workflow.base_brunch", json!("main")),
                ("execution.codex.sandbox", json!("wide-open")),
                ("scoring.enabled", json!("false")),
            ] {
                let before = std::fs::read(&config).unwrap();
                let response = server.send(
                    "PUT",
                    &format!("/api/config/keys/{key}?workspace=ws_http_fixture"),
                    json!({"value":value}),
                );
                assert_eq!(
                    response.status().as_u16(),
                    400,
                    "admission must refuse {key}"
                );
                assert!(response.json::<Value>().unwrap()["error"].is_string());
                assert_eq!(
                    std::fs::read(&config).unwrap(),
                    before,
                    "refused config write must be byte-identical"
                );
            }
            let written = super::support::json_ok(server.send(
                "PUT",
                "/api/config/keys/scoring.enabled?workspace=ws_http_fixture",
                json!({"value":false}),
            ));
            assert_eq!(written["new_value"], false);
            let read = super::support::json_ok(
                server.get("/api/config/effective?workspace=ws_http_fixture"),
            );
            let row = read["sections"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|section| section["keys"].as_array().unwrap())
                .find(|row| row["key"] == "scoring.enabled")
                .unwrap();
            assert_eq!(
                row["value"], false,
                "a boolean config write stays a boolean on HTTP readback"
            );
            assert_eq!(row["source"]["layer"], "workspace");

            let effective_keys = read["sections"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|section| section["keys"].as_array().unwrap())
                .collect::<Vec<_>>();
            for key in ["machine.id", "machine.task_prefix"] {
                let row = effective_keys
                    .iter()
                    .find(|row| row["key"] == key)
                    .unwrap_or_else(|| panic!("effective view includes {key}"));
                assert_eq!(row["settable"], false, "effective write status for {key}");
            }
            let catalog =
                super::support::json_ok(server.get("/api/config/keys?workspace=ws_http_fixture"));
            for key in ["machine.id", "machine.task_prefix"] {
                let row = catalog["keys"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|row| row["key"] == key)
                    .unwrap_or_else(|| panic!("key catalog includes {key}"));
                assert_eq!(row["settable"], false, "catalog write status for {key}");
            }

            // [ORB-13992] The Config tab reports both review switches with
            // their sources, as `orbit config show` and `orbit doctor` do.
            super::support::json_ok(server.send(
                "PUT",
                "/api/config/keys/review.before_pr?workspace=ws_http_fixture",
                json!({"value":true}),
            ));
            let review = &super::support::json_ok(
                server.get("/api/config/effective?workspace=ws_http_fixture"),
            )["review"];
            assert_eq!(review["before_pr"]["enabled"], true, "{review}");
            assert_eq!(review["before_pr"]["source"], "workspace", "{review}");
            assert_eq!(review["before_pr"]["minutes"], 30, "{review}");
            assert_eq!(review["after_landing"]["enabled"], false, "{review}");
            assert!(review["after_landing"]["source"].is_string(), "{review}");
            assert!(review["before_pr"]["line"].is_string(), "{review}");
        },
    );
}
