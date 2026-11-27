//! The Automation › Residents view: an operator sets up a resident mandate
//! from the dashboard alone. Control actions dispatch through their resident
//! tool, and create calls the shared runtime method from the audited dashboard
//! wrapper. Each dashboard write leaves a dashboard audit row beside the history
//! entry core writes.

use orbit_core::app::resident;
use orbit_core::test_support::fixture_store;
use std::fs;

use chrono::{DateTime, Utc};
use orbit_core::test_support::fixture_store::contract::TaskHistoryUpdateParams;
use orbit_types::task::TaskComment;
use orbit_types::telemetry::AuditEventStatus;
use serde_json::{Value, json};

use super::support::{Fixture, add_workspace, error_code, isolated, json_ok};

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
    orbit_core::app::audit::list_audit_events_with_kind(
        &fixture.runtime,
        None,
        None,
        Some(operation.into()),
        Some(status),
        None,
        50,
    )
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
fn resident_reads_report_checkpoint_comment_time_and_keep_empty_state_with_notice() {
    isolated(
        "residents::resident_reads_report_checkpoint_comment_time_and_keep_empty_state_with_notice",
        || {
            let fixture = Fixture::new();
            resident_fixture(&fixture);
            let server = fixture.server(true);

            let checkpoint_id = create(&server, "Report the persisted checkpoint time.");
            let cycle = "rcy-jrun-20261122-1205-checkpoint-time";
            let checkpoint_at: DateTime<Utc> = "2026-11-22T12:24:00Z".parse().unwrap();
            let checkpoint = json!({
                "kind": "resident.checkpoint",
                "schema": 1,
                "mandate": checkpoint_id,
                "cycle": cycle,
                "run_id": "jrun-checkpoint-time",
                "crew": "sol",
                "recovery_of": null,
                "wake": {
                    "reasons": ["heartbeat"],
                    "digest": format!("sha256:{}", "0".repeat(64)),
                },
                "authority": {
                    "rights": [],
                    "observe_only": true,
                    "narrowed_by": null,
                },
                "actions": [],
                "decisions": [],
                "requests": [],
                "costs": [],
                "notes": "",
                "next_wake": {
                    "reason": "heartbeat",
                    "not_before": "2026-11-22T13:00:00Z",
                },
                "continuity": "off",
                "outcome": "idle",
                "probe": false,
                "error": null,
                "conversation_ref": null,
            });
            let registry = fixture_store::maintenance::task_registry::TaskRegistryStore::open(
                &fixture_store::maintenance::task_registry::task_registry_path(
                    &fixture.runtime.global_root(),
                ),
            )
            .unwrap();
            let history = fixture_store::compose::workspace_coordinated_backends(
                registry,
                fixture.runtime.workspace_id().unwrap(),
                fixture.runtime.sqlite_store().unwrap(),
            )
            .unwrap()
            .task
            .history;
            history
                .update_task_history(
                    &checkpoint_id,
                    TaskHistoryUpdateParams {
                        actor: "fixture".into(),
                        append_comments: vec![TaskComment {
                            at: checkpoint_at,
                            by: "fixture".into(),
                            message: checkpoint.to_string(),
                            operation_id: None,
                        }],
                        ..Default::default()
                    },
                )
                .unwrap();

            let empty_id = create(&server, "Keep the empty checkpoint state.");
            json_ok(server.send(
                "POST",
                &format!("/api/residents/{empty_id}/control{WS}"),
                json!({"action": "pause", "reason": "Waiting for a direction."}),
            ));

            let listed = json_ok(server.get(&format!("/api/residents{WS}&all=true")));
            let cards = listed["mandates"].as_array().unwrap();
            let checkpoint_card = cards
                .iter()
                .find(|card| card["task_id"] == checkpoint_id)
                .expect("checkpoint mandate is listed");
            assert_eq!(
                checkpoint_card["latest_checkpoint"]["at"],
                json!(checkpoint_at),
                "checkpoint timestamp comes from the persisted comment, not its 12:05 cycle id: {checkpoint_card}"
            );
            assert_eq!(checkpoint_card["latest_checkpoint"]["cycle"], cycle);
            assert_ne!(
                checkpoint_card["latest_checkpoint"]["at"],
                json!("2026-11-22T12:05:00Z"),
                "the cycle submission minute is not the checkpoint write time"
            );

            let shown = json_ok(server.get(&format!("/api/residents/{checkpoint_id}{WS}")));
            assert_eq!(shown["latest_checkpoint"]["at"], json!(checkpoint_at));

            let empty_card = cards
                .iter()
                .find(|card| card["task_id"] == empty_id)
                .expect("empty mandate is listed");
            assert!(empty_card["latest_checkpoint"].is_null(), "{empty_card}");
            assert!(
                empty_card["status_line"]["title"]
                    .as_str()
                    .unwrap()
                    .starts_with("Paused"),
                "{empty_card}"
            );
            let empty_shown = json_ok(server.get(&format!("/api/residents/{empty_id}{WS}")));
            assert!(empty_shown["latest_checkpoint"].is_null(), "{empty_shown}");
            assert!(
                empty_shown["status_line"]["title"]
                    .as_str()
                    .unwrap()
                    .starts_with("Paused"),
                "{empty_shown}"
            );
        },
    );
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
            assert_eq!(created["status"], "in_progress", "{created}");
            assert_eq!(created["cycle_routine_disabled"], true, "{created}");
            assert_eq!(
                created["cycle_routine_name"], "resident-cycle-fixture",
                "{created}"
            );
            assert!(
                created["warning"].as_str().is_some_and(|warning| {
                    warning.starts_with(&format!("Created mandate {id}, but "))
                        && warning.contains("`resident-cycle-fixture` is disabled")
                }),
                "{created}"
            );
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
            let untouched = resident::show_resident_mandate(&fixture.runtime, &id).unwrap();
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
            let shown = resident::show_resident_mandate(&fixture.runtime, &id).unwrap();
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
            let error = refused["error"].as_str().unwrap_or_default();
            assert!(
                error.starts_with("unknown task complexity: high"),
                "Refusal names invalid complexity 'high': {refused}"
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

            let shown = resident::show_resident_mandate(&fixture.runtime, &id).unwrap();
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
                let shown = resident::show_resident_mandate(&fixture.runtime, id).unwrap();
                assert_eq!(shown["effective_authority"], result["effective_authority"]);
                assert_eq!(shown["mandate"]["deadline"], "2030-01-01T00:00:00Z");
                assert_eq!(shown["authority"]["granted"].as_array().unwrap().len(), 1);
                assert!(
                    shown["authority"]["narrowed"]
                        .as_array()
                        .unwrap()
                        .is_empty()
                );
                let events = orbit_core::app::audit::list_audit_events_with_kind(
                    &fixture.runtime,
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
            let before = resident::show_resident_mandate(&fixture.runtime, &id).unwrap();
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
                let shown = resident::show_resident_mandate(&fixture.runtime, &id).unwrap();
                assert_eq!(shown["authority"], before["authority"], "{shown}");
                assert_eq!(shown["mandate"]["deadline"], "2030-01-01T00:00:00Z");
            }

            let events = orbit_core::app::audit::list_audit_events_with_kind(
                &fixture.runtime,
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
fn every_resident_action_is_audited_at_the_dashboard_boundary() {
    isolated(
        "residents::every_resident_action_is_audited_at_the_dashboard_boundary",
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

            let shown = resident::show_resident_mandate(&fixture.runtime, &id).unwrap();
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

#[test]
fn ended_resident_reads_use_the_ending_event() {
    isolated(
        "residents::ended_resident_reads_use_the_ending_event",
        || {
            let fixture = Fixture::new();
            resident_fixture(&fixture);
            let server = fixture.server(true);
            let archived = create(&server, "Archive after a wake.");
            json_ok(server.send(
                "POST",
                &format!("/api/residents/{archived}/control{WS}"),
                json!({"action": "wake", "reason": "Earlier wake"}),
            ));
            fixture
                .runtime
                .archive_task_as_operator(&archived, false)
                .unwrap();
            let history = fixture.runtime.get_task_history(&archived).unwrap();
            let archive_at = history
                .iter()
                .find(|entry| entry.to_status == Some(orbit_types::task::TaskStatus::Archived))
                .unwrap()
                .at;
            let shown = json_ok(server.get(&format!("/api/residents/{archived}{WS}")));
            assert_eq!(shown["ended_at"], json!(archive_at), "{shown}");
            let stopped = create(&server, "Stop explicitly.");
            let response = json_ok(server.send(
                "POST",
                &format!("/api/residents/{stopped}/control{WS}"),
                json!({"action": "stop", "reason": "Finished"}),
            ));
            let shown = json_ok(server.get(&format!("/api/residents/{stopped}{WS}")));
            assert_eq!(
                shown["ended_at"], response["effective_at"],
                "{shown} {response}"
            );
            let list = json_ok(server.get("/api/residents?all=true&workspace=ws_http_fixture"));
            for (id, expected) in [
                (&archived, json!(archive_at)),
                (&stopped, response["effective_at"].clone()),
            ] {
                let row = list["mandates"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|row| row["task_id"] == *id)
                    .unwrap();
                assert_eq!(row["ended_at"], expected, "{row}");
            }
        },
    );
}

/// The resident list ignores unrelated registered workspaces that do not
/// introduce a loadable same-name collision: a distinct valid routine, a
/// malformed or unresolvable definition (even declaring the same name), and an
/// unreadable routines directory neither change nor erase the selected `routine`
/// row.
#[test]
fn resident_list_routine_ignores_unrelated_registered_workspaces() {
    isolated(
        "residents::resident_list_routine_ignores_unrelated_registered_workspaces",
        || {
            let fixture = Fixture::new();
            resident_fixture(&fixture);
            let server = fixture.server(true);
            let alone_all =
                json_ok(server.get("/api/residents?all=true&workspace=ws_http_fixture"));
            let alone = json_ok(server.get(&format!("/api/residents{WS}")));
            assert!(alone_all["routine"].is_object(), "{alone_all}");

            let template =
                fs::read_to_string(fixture.work.join("routines/resident_cycle.yaml")).unwrap();

            // One workspace holds a distinct valid resident-cycle routine.
            let twin = add_workspace(&fixture, "ws_twin", "twin");
            let twin_routines = twin.paths().orbit_dir.join("routines");
            fs::create_dir_all(&twin_routines).unwrap();
            fs::write(
                twin_routines.join("resident_cycle.yaml"),
                template.replace("name: resident-cycle-fixture", "name: resident-cycle-twin"),
            )
            .unwrap();
            // Another holds a malformed YAML file and an unresolvable definition
            // even declaring the same name; neither loads into the host
            // scheduler, so neither may collide with the selected routine.
            let malformed = add_workspace(&fixture, "ws_malformed", "malformed");
            let malformed_routines = malformed.paths().orbit_dir.join("routines");
            fs::create_dir_all(&malformed_routines).unwrap();
            fs::write(malformed_routines.join("broken.yaml"), "not: [valid: yaml").unwrap();
            fs::write(
                malformed_routines.join("unloadable_same_name.yaml"),
                template.replace(
                    "target: job:resident_cycle_pipeline",
                    "target: job:missing_resident_pipeline",
                ),
            )
            .unwrap();
            // Another has a routines directory that cannot be read.
            let broken = add_workspace(&fixture, "ws_broken", "broken");
            fs::write(broken.paths().orbit_dir.join("routines"), "not a directory").unwrap();

            let with_others_all =
                json_ok(server.get("/api/residents?all=true&workspace=ws_http_fixture"));
            let with_others = json_ok(server.get(&format!("/api/residents{WS}")));
            assert_eq!(with_others_all, alone_all);
            assert_eq!(with_others, alone);
        },
    );
}

/// Two active local workspaces defining loadable routines with the same
/// resident-cycle routine name collide host-wide: the scheduler and `GET
/// /api/routines` drop both, `POST /api/routines/toggle` responds 404, and
/// `GET /api/residents` reflects that rejection by omitting `routine` rather
/// than advertising an un-toggleable row.
#[test]
fn resident_list_drops_colliding_routine_in_sync_with_routines_and_toggle() {
    isolated(
        "residents::resident_list_drops_colliding_routine_in_sync_with_routines_and_toggle",
        || {
            let fixture = Fixture::new();
            resident_fixture(&fixture);
            let server = fixture.server(true);

            // Enable the fixture's cycle routine first: a collision must drop
            // even an enabled definition, matching the host scheduler.
            let before = json_ok(server.get(&format!("/api/residents{WS}")));
            let initial = &before["routine"];
            assert_eq!(initial["enabled"], false, "{before}");
            json_ok(server.send(
                "POST",
                &format!("/api/routines/toggle{WS}"),
                json!({
                    "name": initial["name"],
                    "source": initial["source"],
                    "target": initial["target"],
                    "machine_name": initial["machine_name"],
                    "expected_enabled": false,
                    "enabled": true,
                }),
            ));

            let enabled_residents = json_ok(server.get(&format!("/api/residents{WS}")));
            let enabled_routine = enabled_residents["routine"].clone();
            assert_eq!(enabled_routine["enabled"], true, "{enabled_residents}");
            let routines_before = json_ok(server.get(&format!("/api/routines{WS}")));
            assert!(
                routines_before["routines"]
                    .as_array()
                    .is_some_and(|rows| rows
                        .iter()
                        .any(|row| row["name"] == "resident-cycle-fixture")),
                "{routines_before}"
            );

            // Register a second active local workspace defining a loadable
            // routine with the exact same name and job (even while disabled).
            let twin = add_workspace(&fixture, "ws_twin", "twin");
            let twin_routines = twin.paths().orbit_dir.join("routines");
            fs::create_dir_all(&twin_routines).unwrap();
            fs::copy(
                fixture.work.join("routines/resident_cycle.yaml"),
                twin_routines.join("resident_cycle.yaml"),
            )
            .unwrap();

            let routines_after = json_ok(server.get(&format!("/api/routines{WS}")));
            assert!(
                routines_after["routines"]
                    .as_array()
                    .is_some_and(|rows| rows.is_empty()),
                "Routines should drop the colliding routine: {routines_after}"
            );
            assert!(
                routines_after["load_errors"]
                    .as_array()
                    .is_some_and(|errors| errors.iter().any(|error| {
                        error["message"]
                            .as_str()
                            .is_some_and(|msg| msg.contains("resident-cycle-fixture"))
                    })),
                "Routines should explain the collision in load_errors: {routines_after}"
            );

            error_code(
                server.send(
                    "POST",
                    &format!("/api/routines/toggle{WS}"),
                    json!({
                        "name": enabled_routine["name"],
                        "source": enabled_routine["source"],
                        "target": enabled_routine["target"],
                        "machine_name": enabled_routine["machine_name"],
                        "expected_enabled": true,
                        "enabled": false,
                    }),
                ),
                404,
                "routine_not_found",
            );

            for path in [
                format!("/api/residents{WS}"),
                format!("/api/residents{WS}&all=true"),
            ] {
                let listed = json_ok(server.get(&path));
                assert!(
                    listed["routine"].is_null(),
                    "Residents must omit the dropped colliding routine ({path}): {listed}"
                );
            }

            let scoped = orbit_cmd::registry_routines::routine_statuses_for_workspace(
                &fixture.global,
                "ws_http_fixture",
            )
            .unwrap();
            assert!(
                scoped.statuses.is_empty(),
                "scoped status must omit the colliding routine: {scoped:?}"
            );
            assert!(
                scoped
                    .load_errors
                    .iter()
                    .any(|error| error.message.contains("resident-cycle-fixture")),
                "scoped status must report the collision load error: {scoped:?}"
            );
        },
    );
}

#[test]
fn resident_panel_excludes_resident_tagged_tasks_without_valid_mandate() {
    isolated(
        "residents::resident_panel_excludes_resident_tagged_tasks_without_valid_mandate",
        || {
            let fixture = Fixture::new();
            resident_fixture(&fixture);
            let server = fixture.server(true);

            let real_id = create(&server, "Monitor real resident work.");

            let feature = json_ok(server.send(
                "POST",
                &format!("/api/tasks{WS}"),
                json!({
                    "title": "Feature about residents",
                    "description": "Topic-labelled resident without a mandate.",
                    "complexity": "low",
                    "task_type": "feature",
                    "tags": ["resident"],
                }),
            ));
            let feature_id = feature["id"].as_str().unwrap().to_string();
            assert!(
                feature["warnings"]
                    .as_array()
                    .is_some_and(|warnings| warnings.iter().any(|w| {
                        w.as_str()
                            .is_some_and(|text| text.contains("resident/mandate.json"))
                    })),
                "POST /api/tasks should warn about missing mandate: {feature}"
            );

            let plain = json_ok(server.send(
                "POST",
                &format!("/api/tasks{WS}"),
                json!({
                    "title": "Feature tagged later",
                    "description": "Updated to add the resident tag without a mandate.",
                    "complexity": "low",
                    "task_type": "feature",
                }),
            ));
            let plain_id = plain["id"].as_str().unwrap().to_string();
            let patched = json_ok(server.send(
                "PATCH",
                &format!("/api/tasks/{plain_id}{WS}"),
                json!({"tags": ["resident"]}),
            ));
            assert!(
                patched["warnings"]
                    .as_array()
                    .is_some_and(|warnings| warnings.iter().any(|w| {
                        w.as_str()
                            .is_some_and(|text| text.contains("resident/mandate.json"))
                    })),
                "PATCH /api/tasks/:id should warn about missing mandate: {patched}"
            );

            for path in [
                format!("/api/residents{WS}"),
                format!("/api/residents{WS}&all=true"),
            ] {
                let listed = json_ok(server.get(&path));
                assert_eq!(listed["count"], 1, "{path}: {listed}");
                let mandates = listed["mandates"].as_array().unwrap();
                assert_eq!(mandates.len(), 1, "{path}: {listed}");
                assert_eq!(mandates[0]["task_id"], real_id);
                assert!(
                    !mandates
                        .iter()
                        .any(|m| m["task_id"] == feature_id || m["task_id"] == plain_id),
                    "feature tasks without mandates must not appear in Residents panel: {listed}"
                );
            }
        },
    );
}

/// Rewrites the fixture checkout so its catalog record and the partition ID
/// its `.orbit/config.yaml` persists differ, as an ordinary checkout bound
/// under a different catalog ID does.
fn split_catalog_and_partition_ids(fixture: &Fixture, catalog_id: &str, partition_id: &str) {
    fs::write(
        fixture.work.join("config.yaml"),
        format!("schema_version: 1\nworkspace_id: {partition_id}\n"),
    )
    .unwrap();
    let path = orbit_registry::workspace_registry::registry_path_for(&fixture.global);
    let mut registry = orbit_registry::workspace_registry::load_registry_from(&path).unwrap();
    registry.workspaces[0].id = catalog_id.into();
    registry.checkouts[0].workspace_id = catalog_id.into();
    orbit_registry::workspace_registry::save_registry_to(&registry, &path).unwrap();
}

/// The Residents routine row is selected by the catalog ID the dashboard
/// addresses, not by the partition ID the checkout persists. A twin checkout
/// holding its own resident-cycle routine must not supply the row.
#[test]
fn resident_routine_resolves_catalog_id_when_partition_id_differs() {
    isolated(
        "residents::resident_routine_resolves_catalog_id_when_partition_id_differs",
        || {
            let fixture = Fixture::new();
            resident_fixture(&fixture);
            split_catalog_and_partition_ids(&fixture, "ws_legacy", "ws_storage");
            let server = fixture.server(true);

            let twin = add_workspace(&fixture, "ws_twin", "twin");
            let twin_routines = twin.paths().orbit_dir.join("routines");
            fs::create_dir_all(&twin_routines).unwrap();
            let template =
                fs::read_to_string(fixture.work.join("routines/resident_cycle.yaml")).unwrap();
            fs::write(
                twin_routines.join("resident_cycle.yaml"),
                template.replace("name: resident-cycle-fixture", "name: resident-cycle-twin"),
            )
            .unwrap();

            for path in [
                "/api/residents?workspace=ws_legacy",
                "/api/residents?all=true&workspace=ws_legacy",
            ] {
                let listed = json_ok(server.get(path));
                let routine = &listed["routine"];
                assert!(routine.is_object(), "{path}: {listed}");
                assert_eq!(
                    routine["name"], "resident-cycle-fixture",
                    "{path}: {listed}"
                );
                assert_eq!(routine["source"], "fixture", "{path}: {listed}");
                assert!(routine["machine_name"].is_string(), "{path}: {listed}");
            }
        },
    );
}
