//! The Automation › Residents view: an operator sets up a resident mandate
//! from the dashboard alone, and every write goes through the resident tools
//! with a dashboard audit row beside the history entry core writes.

use std::fs;

use orbit_types::telemetry::AuditEventStatus;
use serde_json::{Value, json};

use super::support::{Fixture, error_code, isolated, json_ok};

const WS: &str = "?workspace=ws_http_fixture";

/// A configured crew and the workspace's seeded resident cycle routine.
fn resident_fixture(fixture: &Fixture) {
    fixture.job("resident_cycle_pipeline");
    let config = fixture.work.join("config.toml");
    let mut text = fs::read_to_string(&config).unwrap_or_default();
    text.push_str("\n[workflow]\ndefault_crew = \"sol\"\n[crews.sol]\nprovider = \"codex\"\nmodel = \"fixture-sol-model\"\n");
    fs::write(&config, text).unwrap();
    let routines = fixture.work.join("routines");
    fs::create_dir_all(&routines).unwrap();
    fs::write(
        routines.join("resident_cycle.yaml"),
        format!(
            "schemaVersion: 1\nname: resident-cycle-fixture\nenabled: false\ntrigger:\n  state:\n    kind: resident_due\n    owner_machine: {}\n    branch: agent-main\n    debounce_minutes: 1\n    max_wait_minutes: 5\n    max_items: 50\n    batch_size: 1\n    retries: 2\n    deadline_minutes: 60\ntarget: job:resident_cycle_pipeline\n",
            fixture.machine_id()
        ),
    )
    .unwrap();
}

fn mandate(goal: &str) -> Value {
    json!({
        "schema": "resident.mandate/v1",
        "goal": goal,
        "exit_criteria": ["Every task tagged release is done."],
        "workspaces": ["fixture"],
        "watch": {"tasks": [], "tags": ["release"]},
        "crew": "sol",
        "cadence": {"floor_minutes": 15, "heartbeat_minutes": 60},
        "continuity": "prefer",
    })
}

fn resident_tasks(fixture: &Fixture) -> usize {
    fixture
        .runtime
        .list_task_metadata()
        .unwrap()
        .iter()
        .filter(|task| task.tags.iter().any(|tag| tag == "resident"))
        .count()
}

fn audits(fixture: &Fixture, operation: &str, status: AuditEventStatus) -> usize {
    fixture
        .runtime
        .list_audit_events_with_kind(None, None, Some(operation.into()), Some(status), None, 50)
        .unwrap()
        .iter()
        .filter(|event| event.command == "dashboard.operations")
        .count()
}

fn create(server: &super::support::Server, goal: &str) -> String {
    let created = json_ok(server.send(
        "POST",
        &format!("/api/residents{WS}"),
        json!({"title": "Keep release green", "complexity": "medium", "mandate": mandate(goal)}),
    ));
    created["task_id"]
        .as_str()
        .expect("created task id")
        .to_string()
}

#[test]
fn dashboard_assignment_preserves_mandate_fields() {
    isolated(
        "residents::dashboard_assignment_preserves_mandate_fields",
        || {
            let fixture = Fixture::new();
            resident_fixture(&fixture);
            let server = fixture.server(true);
            let goal = "A deliberately long goal describing the complete ongoing responsibility of this resident assignment and its desired outcome";
            let created = json_ok(server.send(
                "POST",
                &format!("/api/residents{WS}"),
                json!({"mandate": mandate(goal)}),
            ));
            let id = created["task_id"].as_str().unwrap();
            let task = fixture.runtime.get_task(id).unwrap();
            assert_eq!(task.crew.as_deref(), Some("sol"));
            assert_eq!(
                task.acceptance_criteria,
                vec!["Every task tagged release is done."]
            );
            assert!(task.title.starts_with("[resident]:"), "{}", task.title);
            assert!(
                task.title.chars().count() < goal.chars().count(),
                "{}",
                task.title
            );
        },
    );
}

#[test]
fn operator_sets_up_an_active_mandate_from_the_dashboard_alone() {
    isolated(
        "residents::operator_sets_up_an_active_mandate_from_the_dashboard_alone",
        || {
            let fixture = Fixture::new();
            resident_fixture(&fixture);
            let server = fixture.server(true);

            let listed = json_ok(server.get(&format!("/api/residents{WS}")));
            assert_eq!(listed["count"], 0, "{listed}");
            assert!(
                listed["crews"].as_array().unwrap().contains(&json!("sol")),
                "{listed}"
            );
            assert_eq!(listed["capabilities"]["resident_write"]["authorized"], true);
            let routine = listed["routine"].clone();
            assert_eq!(routine["enabled"], false, "{listed}");

            let id = create(&server, "Keep the release branch green.");
            // A widening without the operator's words is refused before
            // anything is raised or written.
            let unquoted = server.send(
                "POST",
                &format!("/api/residents/{id}/grant{WS}"),
                json!({"add": ["dispatch"], "workspaces": ["fixture"], "quote": "  "}),
            );
            assert_eq!(unquoted.status().as_u16(), 400);
            let unquoted: Value = unquoted.json().unwrap();
            assert_eq!(
                unquoted["error"],
                "a widening needs your words authorizing it; they are recorded verbatim on the grant request",
                "{unquoted}"
            );
            let untouched = fixture.runtime.show_resident_mandate(&id).unwrap();
            assert_eq!(untouched["authority"]["revision"], 0, "{untouched}");
            assert_eq!(untouched["open_requests"], json!([]), "{untouched}");
            assert_eq!(
                audits(&fixture, "resident.grant", AuditEventStatus::Failure),
                1
            );
            let granted = json_ok(server.send(
                "POST",
                &format!("/api/residents/{id}/grant{WS}"),
                json!({"add": ["dispatch", "promote"], "workspaces": ["fixture"],
                       "quote": "Dispatch and promote in fixture."}),
            ));
            assert_eq!(granted["outcome"], "granted", "{granted}");
            let keys = |value: &Value| {
                value
                    .as_object()
                    .unwrap()
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
            };
            assert_eq!(
                keys(&granted),
                ["outcome", "request", "authority", "effective_authority"],
                "an answered widening renders the answered request and the re-read authority"
            );
            assert_eq!(granted["request"]["state"], "answered", "{granted}");
            let toggled = json_ok(server.send(
                "POST",
                &format!("/api/routines/toggle{WS}"),
                json!({"name": routine["name"], "source": routine["source"],
                       "target": routine["target"], "machine_name": routine["machine_name"],
                       "expected_enabled": false, "enabled": true}),
            ));
            assert_eq!(toggled["changed"], true, "{toggled}");

            // `orbit resident show` reads the same tool.
            let shown = fixture.runtime.show_resident_mandate(&id).unwrap();
            assert_eq!(shown["state"], "active", "{shown}");
            assert_eq!(
                shown["effective_authority"],
                json!({"rights": ["dispatch", "promote"], "workspaces": ["fixture"]}),
                "{shown}"
            );
            let grant = &shown["authority"]["granted"][0];
            assert!(
                grant["via"].as_str().unwrap().starts_with("decision:"),
                "a dashboard widening is an answered request: {shown}"
            );
            assert_eq!(grant["quote"], "Dispatch and promote in fixture.");
            let listed = json_ok(server.get(&format!("/api/residents{WS}")));
            assert_eq!(listed["mandates"][0]["task_id"], json!(id), "{listed}");
            assert_eq!(listed["routine"]["enabled"], true, "{listed}");
            assert_eq!(
                audits(&fixture, "resident.create", AuditEventStatus::Success),
                1
            );
            assert_eq!(
                audits(&fixture, "resident.grant", AuditEventStatus::Success),
                1
            );
            let repeated = json_ok(server.send(
                "POST",
                &format!("/api/residents/{id}/grant{WS}"),
                json!({"add": ["dispatch", "promote"], "workspaces": ["fixture"],
                       "quote": "Dispatch and promote in fixture."}),
            ));
            assert_eq!(repeated["outcome"], "unchanged", "{repeated}");
            assert_eq!(
                keys(&repeated),
                ["outcome", "authority"],
                "an already-held widening renders the single grant change"
            );
            let after = json_ok(server.get(&format!("/api/residents/{id}{WS}")));
            assert_eq!(after["authority"], shown["authority"], "{after}");
            assert_eq!(after["effective_authority"], shown["effective_authority"]);
        },
    );
}

#[test]
fn dashboard_creates_every_offered_assessed_complexity() {
    isolated(
        "residents::dashboard_creates_every_offered_assessed_complexity",
        || {
            let fixture = Fixture::new();
            resident_fixture(&fixture);
            let server = fixture.server(true);
            for complexity in ["low", "medium", "hard", "xhard"] {
                let created = json_ok(server.send(
                    "POST",
                    &format!("/api/residents{WS}"),
                    json!({"complexity": complexity, "mandate": mandate("Keep release green.")}),
                ));
                let id = created["task_id"].as_str().unwrap();
                let task = json_ok(server.get(&format!("/api/tasks/{id}{WS}")));
                assert_eq!(task["complexity"], complexity, "{task}");
                assert_eq!(task["status"], "in-progress", "{task}");
            }
            let refused = server.send(
                "POST",
                &format!("/api/residents{WS}"),
                json!({"complexity": "high", "mandate": mandate("Invalid tier.")}),
            );
            assert_eq!(refused.status().as_u16(), 400);
            let refused: Value = refused.json().unwrap();
            assert_eq!(
                refused["error"],
                "unknown task complexity: high (valid complexities: low, medium, hard, xhard)",
                "{refused}"
            );
            assert_eq!(
                resident_tasks(&fixture),
                4,
                "Invalid complexity creates nothing"
            );
        },
    );
}

#[test]
fn dashboard_mixed_grant_reports_unchanged_narrowing() {
    isolated(
        "residents::dashboard_mixed_grant_reports_unchanged_narrowing",
        || {
            let fixture = Fixture::new();
            resident_fixture(&fixture);
            let server = fixture.server(true);
            let id = create(&server, "Report unchanged narrowing.");
            let path = format!("/api/residents/{id}/grant{WS}");
            let body = json!({"add": ["dispatch"], "workspaces": ["fixture"],
                          "remove": ["promote"], "quote": "Dispatch in fixture."});

            let applied = json_ok(server.send("POST", &path, body.clone()));
            assert_eq!(applied["outcome"], "granted", "{applied}");
            let before = json_ok(server.get(&format!("/api/residents/{id}{WS}")));
            assert_eq!(before["authority"]["revision"], 1, "{before}");
            assert_eq!(before["authority"]["granted"].as_array().unwrap().len(), 1);
            assert!(
                before["authority"]["narrowed"]
                    .as_array()
                    .unwrap()
                    .is_empty()
            );

            let unchanged = json_ok(server.send("POST", &path, body));
            assert_eq!(unchanged["outcome"], "unchanged", "{unchanged}");
            assert!(unchanged["request"].is_null(), "{unchanged}");
            let after = json_ok(server.get(&format!("/api/residents/{id}{WS}")));
            assert_eq!(after["authority"], before["authority"], "{after}");
            assert_eq!(after["effective_authority"], before["effective_authority"]);
        },
    );
}

#[test]
fn dashboard_grant_with_add_and_deadline_records_two_changes() {
    isolated(
        "residents::dashboard_grant_with_add_and_deadline_records_two_changes",
        || {
            let fixture = Fixture::new();
            resident_fixture(&fixture);
            let server = fixture.server(true);

            let id = create(&server, "Widen with a deadline.");
            // The dashboard once sent this as one mixed call, which core
            // refuses; it must succeed as two recorded changes instead.
            let granted = json_ok(server.send(
                "POST",
                &format!("/api/residents/{id}/grant{WS}"),
                json!({
                    "add": ["dispatch"],
                    "workspaces": ["fixture"],
                    "deadline": "2030-01-01T00:00:00Z",
                    "quote": "Dispatch in fixture with a deadline.",
                }),
            ));
            assert_eq!(granted["outcome"], "granted_and_narrowed", "{granted}");

            let shown = fixture.runtime.show_resident_mandate(&id).unwrap();
            assert_eq!(
                shown["effective_authority"],
                json!({"rights": ["dispatch"], "workspaces": ["fixture"]}),
                "{shown}"
            );
            assert_eq!(
                shown["mandate"]["deadline"], "2030-01-01T00:00:00Z",
                "{shown}"
            );
            assert_eq!(
                shown["authority"]["granted"].as_array().unwrap().len(),
                1,
                "{shown}"
            );
            assert_eq!(
                shown["authority"]["narrowed"].as_array().unwrap().len(),
                1,
                "{shown}"
            );
            assert_eq!(
                shown["authority"]["narrowed"][0]["deadline"], "2030-01-01T00:00:00Z",
                "{shown}"
            );
        },
    );
}

#[test]
fn dashboard_mixed_grant_reports_applied_widening_when_deadline_is_refused() {
    isolated(
        "residents::dashboard_mixed_grant_reports_applied_widening_when_deadline_is_refused",
        || {
            let fixture = Fixture::new();
            resident_fixture(&fixture);
            let server = fixture.server(true);

            for (deadline, reason) in [
                ("2031-01-01T00:00:00Z", "only shortens the mandate"),
                ("2030-01-01T00:00:00Z", "only shortens the mandate"),
                ("not-a-timestamp", "must be an RFC 3339 timestamp"),
            ] {
                let mut draft = mandate("Report the applied widening.");
                draft["deadline"] = json!("2030-01-01T00:00:00Z");
                let created = json_ok(server.send(
                    "POST",
                    &format!("/api/residents{WS}"),
                    json!({"mandate": draft}),
                ));
                let id = created["task_id"].as_str().unwrap();
                let result = json_ok(server.send(
                    "POST",
                    &format!("/api/residents/{id}/grant{WS}"),
                    json!({"add": ["dispatch"], "workspaces": ["fixture"],
                           "quote": "Dispatch in fixture.", "deadline": deadline}),
                ));
                assert_eq!(result["outcome"], "granted_narrowing_refused", "{result}");
                assert!(
                    result["narrowing_error"].as_str().unwrap().contains(reason),
                    "{result}"
                );
                assert_eq!(
                    result["effective_authority"],
                    json!({"rights": ["dispatch"], "workspaces": ["fixture"]}),
                    "{result}"
                );
                let shown = fixture.runtime.show_resident_mandate(id).unwrap();
                assert_eq!(shown["effective_authority"], result["effective_authority"]);
                assert_eq!(shown["mandate"]["deadline"], "2030-01-01T00:00:00Z");
                assert_eq!(shown["authority"]["granted"].as_array().unwrap().len(), 1);
                assert!(
                    shown["authority"]["narrowed"]
                        .as_array()
                        .unwrap()
                        .is_empty()
                );
                let events = fixture
                    .runtime
                    .list_audit_events_with_kind(
                        None,
                        None,
                        Some("resident.grant".into()),
                        Some(AuditEventStatus::Failure),
                        None,
                        50,
                    )
                    .unwrap();
                let event = events
                    .iter()
                    .find(|event| {
                        event.command == "dashboard.operations"
                            && event.target_id.as_deref() == Some(id)
                    })
                    .expect("partial outcome is audited");
                let message = event.error_message.as_deref().unwrap();
                assert!(
                    message.contains("Widening applied; narrowing refused"),
                    "{message}"
                );
                assert!(message.contains(reason), "{message}");
            }
        },
    );
}

#[test]
fn dashboard_mixed_grant_with_already_held_widening_reports_unchanged_when_deadline_is_refused() {
    isolated(
        "residents::dashboard_mixed_grant_with_already_held_widening_reports_unchanged_when_deadline_is_refused",
        || {
            let fixture = Fixture::new();
            resident_fixture(&fixture);
            let server = fixture.server(true);

            let mut draft = mandate("Already-held widening with a refused deadline.");
            draft["deadline"] = json!("2030-01-01T00:00:00Z");
            let created = json_ok(server.send(
                "POST",
                &format!("/api/residents{WS}"),
                json!({"mandate": draft}),
            ));
            let id = created["task_id"].as_str().unwrap().to_string();
            let granted = json_ok(server.send(
                "POST",
                &format!("/api/residents/{id}/grant{WS}"),
                json!({"add": ["dispatch"], "workspaces": ["fixture"],
                       "quote": "Dispatch in fixture."}),
            ));
            assert_eq!(granted["outcome"], "granted", "{granted}");
            let before = fixture.runtime.show_resident_mandate(&id).unwrap();
            assert_eq!(before["authority"]["revision"], 1, "{before}");
            assert_eq!(before["mandate"]["deadline"], "2030-01-01T00:00:00Z");

            for deadline in ["2031-01-01T00:00:00Z", "2030-01-01T00:00:00Z"] {
                let result = json_ok(server.send(
                    "POST",
                    &format!("/api/residents/{id}/grant{WS}"),
                    json!({"add": ["dispatch"], "workspaces": ["fixture"],
                           "quote": "Dispatch in fixture.", "deadline": deadline}),
                ));
                assert_eq!(result["outcome"], "unchanged_narrowing_refused", "{result}");
                assert!(
                    result["narrowing_error"]
                        .as_str()
                        .unwrap()
                        .contains("only shortens the mandate"),
                    "{result}"
                );
                assert!(
                    result["request"].is_null(),
                    "an unchanged widening raises and answers no request: {result}"
                );
                assert_eq!(
                    result["effective_authority"],
                    json!({"rights": ["dispatch"], "workspaces": ["fixture"]}),
                    "{result}"
                );
                let shown = fixture.runtime.show_resident_mandate(&id).unwrap();
                assert_eq!(shown["authority"], before["authority"], "{shown}");
                assert_eq!(shown["mandate"]["deadline"], "2030-01-01T00:00:00Z");
            }

            let events = fixture
                .runtime
                .list_audit_events_with_kind(
                    None,
                    None,
                    Some("resident.grant".into()),
                    Some(AuditEventStatus::Failure),
                    None,
                    50,
                )
                .unwrap();
            let messages: Vec<&str> = events
                .iter()
                .filter(|event| {
                    event.command == "dashboard.operations"
                        && event.target_id.as_deref() == Some(id.as_str())
                })
                .filter_map(|event| event.error_message.as_deref())
                .collect();
            assert_eq!(messages.len(), 2, "{messages:?}");
            for message in messages {
                assert!(
                    message.contains("No widening applied; narrowing refused"),
                    "{message}"
                );
                assert!(!message.contains("Widening applied"), "{message}");
            }
        },
    );
}

#[test]
fn invalid_mandate_forms_are_refused_with_the_server_message_and_write_nothing() {
    isolated(
        "residents::invalid_mandate_forms_are_refused_with_the_server_message_and_write_nothing",
        || {
            let fixture = Fixture::new();
            resident_fixture(&fixture);
            let server = fixture.server(true);
            let mut nine = mandate("Goal.");
            nine["exit_criteria"] = json!(
                (0..9)
                    .map(|n| format!("Criterion {n}."))
                    .collect::<Vec<_>>()
            );
            let mut slow = mandate("Goal.");
            slow["cadence"] = json!({"floor_minutes": 30, "heartbeat_minutes": 10});
            let mut crew = mandate("Goal.");
            crew["crew"] = json!("nobody");
            for (draft, message) in [
                (
                    mandate(""),
                    "mandate `goal` must be one line of at most 240 characters",
                ),
                (nine, "a mandate holds 1-8 exit criteria, not 9"),
                (
                    slow,
                    "mandate `cadence.heartbeat_minutes` must be at least `cadence.floor_minutes`",
                ),
                (crew, "mandate crew `nobody` is not a configured crew"),
            ] {
                let response = server.send(
                    "POST",
                    &format!("/api/residents{WS}"),
                    json!({"title": "Refused", "mandate": draft}),
                );
                assert_eq!(response.status().as_u16(), 400);
                let body: Value = response.json().unwrap();
                assert_eq!(body["error"], message, "{body}");
            }
            assert_eq!(
                resident_tasks(&fixture),
                0,
                "a refused form creates nothing"
            );

            let agent = fixture.server(false);
            error_code(
                agent.send(
                    "POST",
                    &format!("/api/residents{WS}"),
                    json!({"mandate": mandate("Goal.")}),
                ),
                403,
                "authorization_denied",
            );
            assert_eq!(resident_tasks(&fixture), 0);
            assert_eq!(
                audits(&fixture, "resident.create", AuditEventStatus::Denied),
                1
            );
        },
    );
}

#[test]
fn every_resident_action_runs_through_its_tool_and_is_audited() {
    isolated(
        "residents::every_resident_action_runs_through_its_tool_and_is_audited",
        || {
            let fixture = Fixture::new();
            resident_fixture(&fixture);
            let server = fixture.server(true);
            let id = create(&server, "Steer from the dashboard.");
            json_ok(server.send(
                "POST",
                &format!("/api/residents/{id}/grant{WS}"),
                json!({"add": ["dispatch", "promote"], "workspaces": ["fixture"], "quote": "Go."}),
            ));
            let control = |action: &str, extra: Value| {
                let mut body =
                    json!({"action": action, "reason": format!("{action} from the view")});
                for (key, value) in extra.as_object().unwrap() {
                    body[key] = value.clone();
                }
                json_ok(server.send("POST", &format!("/api/residents/{id}/control{WS}"), body))
            };
            assert_eq!(control("pause", json!({}))["state"], "paused");
            assert_eq!(control("resume", json!({}))["state"], "active");
            assert_eq!(
                control("wake", json!({"status_request": true}))["state"],
                "active"
            );
            assert_eq!(
                control("takeover", json!({"by_session": "dashboard-operator"}))["state"],
                "taken_over"
            );
            assert_eq!(control("release", json!({}))["state"], "active");

            let narrowed = json_ok(server.send(
                "POST",
                &format!("/api/residents/{id}/grant{WS}"),
                json!({"remove": ["promote"]}),
            ));
            assert_eq!(narrowed["outcome"], "narrowed", "{narrowed}");
            json_ok(server.send(
                "POST",
                &format!("/api/residents/{id}/relay{WS}"),
                json!({"quote": "Ship CSV first."}),
            ));

            // A request another caller raised is answered from the view's
            // decision action, which an agent session may not use.
            let raised = fixture
                .runtime
                .run_tool(
                    "orbit.resident.grant",
                    json!({"task_id": id, "add": ["merge"], "quote": "Merge reviewed PRs."}),
                )
                .unwrap();
            assert_eq!(raised["outcome"], "requested", "{raised}");
            let request = raised["request"]["id"].as_str().unwrap().to_string();
            let shown = json_ok(server.get(&format!("/api/residents/{id}{WS}")));
            assert_eq!(shown["open_requests"][0]["id"], json!(request), "{shown}");
            let agent = fixture.server(false);
            error_code(
                agent.send(
                    "POST",
                    &format!("/api/decisions/{request}/answer{WS}"),
                    json!({"answer": "grant"}),
                ),
                403,
                "authorization_denied",
            );
            json_ok(server.send(
                "POST",
                &format!("/api/decisions/{request}/answer{WS}"),
                json!({"answer": "grant"}),
            ));
            assert_eq!(control("stop", json!({}))["state"], "ended");

            let shown = fixture.runtime.show_resident_mandate(&id).unwrap();
            assert_eq!(shown["state"], "ended", "{shown}");
            assert_eq!(shown["status"], "review", "{shown}");
            assert_eq!(
                shown["authority"]["rights"],
                json!(["dispatch", "merge"]),
                "{shown}"
            );
            let history = fixture.runtime.get_task_history(&id).unwrap();
            let controls = history
                .iter()
                .filter(|entry| entry.event == "resident_control")
                .count();
            assert_eq!(controls, 6, "each control action is a history entry");
            for (operation, count) in [
                ("resident.control", 6),
                ("resident.narrow", 1),
                ("resident.relay", 1),
            ] {
                assert_eq!(
                    audits(&fixture, operation, AuditEventStatus::Success),
                    count,
                    "{operation}"
                );
            }
        },
    );
}
