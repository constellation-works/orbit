//! Plugin page actions (docs/design/plugin-pages/2_design.md §1, §3): a
//! mutating tool a page names under `actions` runs only with the operator
//! session, the operator's consent to the list, and a one-time confirmation
//! issued for exactly that plugin, page, tool and input. Every run request,
//! refused or run, is audited with the web session.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use orbit_core::adapter::command::{PluginAddOptions, PluginEnableOptions};
use orbit_core::app::plugin;
use orbit_types::telemetry::{AuditEvent, AuditEventStatus};
use serde_json::{Value, json};

use super::support::{Fixture, Server, isolated, json_ok};

const WS: &str = "workspace=ws_http_fixture";

const NS: &str = "acts";

/// Unused confirmations the server holds at once.
const CONFIRMATION_SLOTS: u64 = 32;

const WITH_ACTIONS: &str = "        actions: [mutate]\n";

/// A backend that records each run in `<control>/calls` and echoes its
/// request.
fn backend(control: &Path) -> String {
    let control = control.display();
    format!(
        "#!/bin/sh\nrequest=$(cat)\necho run >> '{control}/calls'\nprintf \
         '{{\"ok\":true,\"output\":{{\"request\":%s}}}}\\n' \"$request\"\n"
    )
}

/// Write the plugin source; `actions` is the page's `actions` line, or
/// empty for a page without one.
fn write_source(fixture: &Fixture, actions: &str) -> (PathBuf, PathBuf) {
    let control = fixture.path("control");
    fs::create_dir_all(&control).unwrap();
    let source = fixture.path("source/.orbit-plugin");
    let backend_path = source.join("bin/backend.sh");
    fs::create_dir_all(backend_path.parent().unwrap()).unwrap();
    fs::write(&backend_path, backend(&control)).unwrap();
    fs::set_permissions(&backend_path, fs::Permissions::from_mode(0o755)).unwrap();
    fs::create_dir_all(source.join("web/overview")).unwrap();
    fs::write(source.join("web/overview/index.html"), "<main></main>").unwrap();
    let manifest = [
        "schemaVersion: 2",
        "kind: Plugin",
        "metadata:",
        &format!("  name: {NS}"),
        "  version: 0.1.0",
        "  description: Action fixture.",
        "spec:",
        "  backend:",
        "    type: exec",
        "    command: bin/backend.sh",
        "    sandbox: none",
        "  tools:",
        "    - name: status",
        "      description: Read status.",
        "      execution_kind: read_only",
        "      mcp_scope: workspace",
        "    - name: mutate",
        "      description: Change something.",
        "      execution_kind: mutating",
        "      mcp_scope: workspace",
        "      input_schema:",
        "        type: object",
        "        properties:",
        "          value:",
        "            type: integer",
        "        additionalProperties: false",
        "  web:",
        "    pages:",
        "      - id: overview",
        "        title: Overview",
        "        root: web/overview",
        "        tools: [status]",
    ]
    .join("\n");
    fs::write(source.join("plugin.yaml"), format!("{manifest}\n{actions}")).unwrap();
    (source, control)
}

fn add(fixture: &Fixture, source: &Path, force: bool) {
    plugin::add_plugin(
        &fixture.runtime,
        source.to_str().unwrap(),
        &PluginAddOptions {
            force,
            ..PluginAddOptions::default()
        },
    )
    .unwrap();
}

fn enable(fixture: &Fixture) {
    plugin::enable_plugin(
        &fixture.runtime,
        NS,
        &PluginEnableOptions {
            grants: vec!["unsandboxed".into()],
            force: false,
        },
    )
    .unwrap();
}

fn install(fixture: &Fixture) -> PathBuf {
    let (source, control) = write_source(fixture, WITH_ACTIONS);
    add(fixture, &source, false);
    enable(fixture);
    control
}

fn confirm(server: &Server, tool: &str, input: Value) -> reqwest::blocking::Response {
    server.send(
        "POST",
        &format!("/api/plugins/{NS}/pages/overview/actions/confirm?{WS}"),
        json!({"tool": tool, "input": input}),
    )
}

fn run(server: &Server, body: Value) -> reqwest::blocking::Response {
    server.send(
        "POST",
        &format!("/api/plugins/{NS}/pages/overview/actions/run?{WS}"),
        body,
    )
}

fn cancel(server: &Server, body: Value) -> Value {
    json_ok(server.send(
        "POST",
        &format!("/api/plugins/{NS}/pages/overview/actions/cancel?{WS}"),
        body,
    ))
}

fn confirmation(server: &Server, input: Value) -> String {
    let checked = json_ok(confirm(server, "mutate", input));
    checked["confirmation"]
        .as_str()
        .unwrap_or_else(|| panic!("a confirmation: {checked}"))
        .to_string()
}

/// The refusal's body, after checking its status and code; `case` names
/// the attempt in a failure.
fn refused(response: reqwest::blocking::Response, status: u16, code: &str, case: &str) -> Value {
    let actual = response.status().as_u16();
    let value: Value = response.json().unwrap();
    assert_eq!(actual, status, "{case}: {value}");
    assert_eq!(value["code"], json!(code), "{case}: {value}");
    value
}

fn run_count(control: &Path) -> usize {
    fs::read_to_string(control.join("calls"))
        .map(|text| text.lines().count())
        .unwrap_or(0)
}

/// The action rows, newest first.
fn action_rows(fixture: &Fixture) -> Vec<AuditEvent> {
    orbit_core::app::audit::list_audit_events_with_kind(
        &fixture.runtime,
        None,
        None,
        Some("plugin.page_action".into()),
        None,
        None,
        100,
    )
    .unwrap()
    .into_iter()
    .filter(|event| event.command == "dashboard.operations")
    .collect()
}

fn assert_rows_carry_session_plugin_and_tool(rows: &[AuditEvent]) {
    for row in rows {
        let session = row.session_id.as_deref().unwrap_or_default();
        assert!(session.starts_with("web-operator-"), "{row:?}");
        assert_eq!(row.target_id.as_deref(), Some(NS), "{row:?}");
        let arguments: Value =
            serde_json::from_str(row.arguments_json.as_deref().unwrap_or("null")).unwrap();
        assert_eq!(arguments["tool"], "mutate", "{row:?}");
        assert_eq!(arguments["page"], "overview", "{row:?}");
    }
}

#[test]
fn an_action_runs_once_only_with_a_confirmation_for_its_exact_input() {
    isolated(
        "plugin_page_action::an_action_runs_once_only_with_a_confirmation_for_its_exact_input",
        || {
            let fixture = Fixture::new();
            let control = install(&fixture);
            let server = fixture.server(true);

            // The listing names the action as consented and active.
            let plugins = json_ok(server.get(&format!("/api/plugins?{WS}")));
            let listed = plugins
                .as_array()
                .unwrap()
                .iter()
                .find(|plugin| plugin["name"] == NS)
                .unwrap();
            assert_eq!(
                listed["pages"][0]["actions"],
                json!([{"tool": "acts.mutate", "consented": true, "active": true}]),
                "{listed}"
            );

            // The check names what the dialog shows and runs nothing.
            let checked = json_ok(confirm(&server, "mutate", json!({"value": 1})));
            assert_eq!(checked["plugin"], NS);
            assert_eq!(checked["version"], "0.1.0");
            assert_eq!(checked["tool"], "acts.mutate");
            assert_eq!(checked["input"], json!({"value": 1}));
            assert_eq!(run_count(&control), 0, "a check runs nothing");

            // Straight to the server without a confirmation.
            refused(
                run(&server, json!({"tool": "mutate", "input": {"value": 1}})),
                403,
                "confirmation_refused",
                "no confirmation",
            );
            refused(
                run(
                    &server,
                    json!({"tool": "mutate", "input": {"value": 1}, "confirmation": "forged"}),
                ),
                403,
                "confirmation_refused",
                "an unknown confirmation",
            );
            // A confirmation issued for another input, which is spent by
            // the attempt.
            let token = checked["confirmation"].as_str().unwrap().to_string();
            refused(
                run(
                    &server,
                    json!({"tool": "mutate", "input": {"value": 2}, "confirmation": token}),
                ),
                403,
                "confirmation_refused",
                "a confirmation for another input",
            );
            refused(
                run(
                    &server,
                    json!({"tool": "mutate", "input": {"value": 1}, "confirmation": token}),
                ),
                403,
                "confirmation_refused",
                "a confirmation spent by a refused request",
            );
            assert_eq!(run_count(&control), 0, "no refused request ran the tool");

            // The confirmed action runs once, and its confirmation is spent.
            let token = confirmation(&server, json!({"value": 1}));
            let body = json!({"tool": "mutate", "input": {"value": 1}, "confirmation": token});
            let output = json_ok(run(&server, body.clone()));
            assert!(
                output["output"]["request"]
                    .to_string()
                    .contains("\"value\":1"),
                "{output}"
            );
            assert_eq!(run_count(&control), 1);
            refused(
                run(&server, body),
                403,
                "confirmation_refused",
                "a replayed confirmation",
            );
            assert_eq!(run_count(&control), 1, "a replay runs nothing");

            // What the check refuses is never confirmed.
            for (tool, input, status, code) in [
                ("status", json!({}), 403, "tool_refused"),
                ("other.mutate", json!({}), 403, "tool_refused"),
                ("mutate", json!({"value": "x"}), 400, "input_invalid"),
            ] {
                refused(confirm(&server, tool, input), status, code, tool);
            }
            assert_eq!(run_count(&control), 1);

            // Every run request has a row with the web session, the plugin
            // and the tool: five refused, one run.
            let rows = action_rows(&fixture);
            assert_eq!(rows.len(), 6, "{rows:#?}");
            assert_eq!(
                rows.iter()
                    .filter(|row| row.status == AuditEventStatus::Success)
                    .count(),
                1,
                "{rows:#?}"
            );
            assert_rows_carry_session_plugin_and_tool(&rows);
            // The tool call itself is audited with the web session too.
            let tool_rows = orbit_core::app::audit::list_audit_events_with_kind(
                &fixture.runtime,
                None,
                Some("acts.mutate".into()),
                None,
                Some(AuditEventStatus::Success),
                None,
                10,
            )
            .unwrap();
            assert_eq!(tool_rows.len(), 1, "{tool_rows:#?}");
            assert_eq!(
                tool_rows[0].origin_session_id, rows[0].session_id,
                "{tool_rows:#?}"
            );

            // A read-only call through `tools` is unchanged.
            let read = json_ok(server.send(
                "POST",
                &format!("/api/plugins/{NS}/pages/overview/call?{WS}"),
                json!({"tool": "status", "input": {}}),
            ));
            assert!(read["output"].is_object(), "{read}");
            refused(
                server.send(
                    "POST",
                    &format!("/api/plugins/{NS}/pages/overview/call?{WS}"),
                    json!({"tool": "mutate", "input": {}}),
                ),
                403,
                "tool_refused",
                "a call to an action through `call`",
            );
            assert_eq!(run_count(&control), 2, "only the read ran");
        },
    );
}

#[test]
fn a_cancelled_or_abandoned_confirmation_frees_its_slot_and_runs_nothing() {
    isolated(
        "plugin_page_action::a_cancelled_or_abandoned_confirmation_frees_its_slot_and_runs_nothing",
        || {
            let fixture = Fixture::new();
            let control = install(&fixture);
            let server = fixture.server(true);

            // Abandoned confirmations fill every slot; the next check is busy.
            let tokens: Vec<String> = (0..CONFIRMATION_SLOTS)
                .map(|value| confirmation(&server, json!({"value": value})))
                .collect();
            refused(
                confirm(&server, "mutate", json!({"value": 99})),
                429,
                "busy",
                "every slot held",
            );

            // A release for another input frees nothing.
            let released = cancel(
                &server,
                json!({"tool": "mutate", "input": {"value": 99}, "confirmation": tokens[0]}),
            );
            assert_eq!(released["released"], false, "{released}");
            refused(
                confirm(&server, "mutate", json!({"value": 99})),
                429,
                "busy",
                "a release for another input",
            );

            // Releasing one frees exactly one slot at once, and only once.
            let body = json!({"tool": "mutate", "input": {"value": 0}, "confirmation": tokens[0]});
            assert_eq!(cancel(&server, body.clone())["released"], true);
            assert_eq!(
                cancel(&server, body.clone())["released"],
                false,
                "a second release is a no-op"
            );
            let fresh = confirmation(&server, json!({"value": 99}));
            refused(
                confirm(&server, "mutate", json!({"value": 98})),
                429,
                "busy",
                "the freed slot is taken again",
            );

            // The released confirmation no longer runs anything.
            refused(
                run(&server, body),
                403,
                "confirmation_refused",
                "a released confirmation",
            );
            assert_eq!(run_count(&control), 0, "a cancel runs nothing");

            // Releasing the rest leaves room, and a valid action still runs.
            for (value, token) in tokens.iter().enumerate().skip(1) {
                let released = cancel(
                    &server,
                    json!({"tool": "mutate", "input": {"value": value}, "confirmation": token}),
                );
                assert_eq!(released["released"], true, "{released}");
            }
            json_ok(run(
                &server,
                json!({"tool": "mutate", "input": {"value": 99}, "confirmation": fresh}),
            ));
            assert_eq!(run_count(&control), 1, "only the confirmed action ran");
            let later = confirmation(&server, json!({"value": 7}));
            assert!(!later.is_empty());
        },
    );
}

#[test]
fn without_the_operator_session_an_action_is_refused_and_audited() {
    isolated(
        "plugin_page_action::without_the_operator_session_an_action_is_refused_and_audited",
        || {
            let fixture = Fixture::new();
            let control = install(&fixture);
            let server = fixture.server(false);

            refused(
                confirm(&server, "mutate", json!({"value": 1})),
                403,
                "authorization_denied",
                "a check without the operator session",
            );
            refused(
                run(
                    &server,
                    json!({"tool": "mutate", "input": {"value": 1}, "confirmation": "forged"}),
                ),
                403,
                "authorization_denied",
                "a run without the operator session",
            );
            assert_eq!(run_count(&control), 0);

            let rows = action_rows(&fixture);
            assert_eq!(rows.len(), 1, "{rows:#?}");
            assert_eq!(rows[0].status, AuditEventStatus::Denied, "{rows:#?}");
            assert_rows_carry_session_plugin_and_tool(&rows);
        },
    );
}

#[test]
fn an_action_added_since_consent_is_inactive_until_consent_is_given_again() {
    isolated(
        "plugin_page_action::an_action_added_since_consent_is_inactive_until_consent_is_given_again",
        || {
            let fixture = Fixture::new();
            let (source, control) = write_source(&fixture, "");
            add(&fixture, &source, false);
            enable(&fixture);
            // The new manifest names an action; a plain forced reinstall
            // keeps the plugin enabled but records no consent for it.
            write_source(&fixture, WITH_ACTIONS);
            add(&fixture, &source, true);

            let server = fixture.server(true);
            let plugins = json_ok(server.get(&format!("/api/plugins?{WS}")));
            let listed = plugins
                .as_array()
                .unwrap()
                .iter()
                .find(|plugin| plugin["name"] == NS)
                .unwrap();
            assert_eq!(listed["status"], "active", "{listed}");
            assert_eq!(
                listed["pages"][0]["actions"],
                json!([{"tool": "acts.mutate", "consented": false, "active": false}]),
                "{listed}"
            );
            let body = refused(
                confirm(&server, "mutate", json!({"value": 1})),
                403,
                "tool_refused",
                "an action without recorded consent",
            );
            assert!(
                body["error"]
                    .as_str()
                    .unwrap()
                    .contains("orbit plugin enable acts"),
                "{body}"
            );
            // The dashboard's re-enable keeps the consent it had.
            json_ok(server.send(
                "POST",
                &format!("/api/plugins/{NS}/enable?{WS}"),
                json!({"scope": "host"}),
            ));
            refused(
                confirm(&server, "mutate", json!({"value": 1})),
                403,
                "tool_refused",
                "after the dashboard's re-enable",
            );
            drop(server);

            enable(&fixture);
            let server = fixture.server(true);
            let token = confirmation(&server, json!({"value": 1}));
            json_ok(run(
                &server,
                json!({"tool": "mutate", "input": {"value": 1}, "confirmation": token}),
            ));
            assert_eq!(run_count(&control), 1);
        },
    );
}

#[test]
fn an_invalid_action_run_body_spends_its_confirmation_and_is_audited() {
    isolated(
        "plugin_page_action::an_invalid_action_run_body_spends_its_confirmation_and_is_audited",
        || {
            let fixture = Fixture::new();
            let control = install(&fixture);
            let server = fixture.server(true);

            // Missing `tool` with a valid confirmation spends the token and
            // blocks any later retry with it.
            let missing_tool_token = confirmation(&server, json!({"value": 1}));
            refused(
                run(
                    &server,
                    json!({"input": {"value": 1}, "confirmation": missing_tool_token}),
                ),
                403,
                "confirmation_refused",
                "missing tool with valid confirmation",
            );
            refused(
                run(
                    &server,
                    json!({"tool": "mutate", "input": {"value": 1}, "confirmation": missing_tool_token}),
                ),
                403,
                "confirmation_refused",
                "retry after missing-tool refusal spent the confirmation",
            );

            // Non-string `tool` with a valid confirmation spends the token too.
            let non_string_tool_token = confirmation(&server, json!({"value": 1}));
            refused(
                run(
                    &server,
                    json!({"tool": 123, "input": {"value": 1}, "confirmation": non_string_tool_token}),
                ),
                403,
                "confirmation_refused",
                "non-string tool with valid confirmation",
            );
            refused(
                run(
                    &server,
                    json!({"tool": "mutate", "input": {"value": 1}, "confirmation": non_string_tool_token}),
                ),
                403,
                "confirmation_refused",
                "retry after non-string-tool refusal spent the confirmation",
            );

            // A non-object JSON body is also refused and audited.
            refused(
                run(&server, json!(["not", "an", "object"])),
                403,
                "confirmation_refused",
                "non-object body",
            );
            assert_eq!(
                run_count(&control),
                0,
                "no invalid request or retry ran the tool"
            );

            // Every refused attempt left an audit row with session, plugin,
            // and available action metadata (page, plus tool on retries).
            let rows = action_rows(&fixture);
            assert_eq!(rows.len(), 5, "{rows:#?}");
            for (index, row) in rows.iter().enumerate() {
                assert_eq!(row.status, AuditEventStatus::Failure, "{row:?}");
                let session = row.session_id.as_deref().unwrap_or_default();
                assert!(session.starts_with("web-operator-"), "{row:?}");
                assert_eq!(row.target_id.as_deref(), Some(NS), "{row:?}");
                let arguments: Value =
                    serde_json::from_str(row.arguments_json.as_deref().unwrap_or("null")).unwrap();
                assert_eq!(arguments["page"], "overview", "{row:?}");
                // Newest first: indices 1 and 3 are the two retry rows.
                if matches!(index, 1 | 3) {
                    assert_eq!(arguments["tool"], "mutate", "{row:?}");
                } else {
                    assert!(arguments.get("tool").is_none(), "{row:?}");
                }
            }

            // A fresh valid confirmation still executes once.
            let fresh = confirmation(&server, json!({"value": 2}));
            json_ok(run(
                &server,
                json!({"tool": "mutate", "input": {"value": 2}, "confirmation": fresh}),
            ));
            assert_eq!(run_count(&control), 1);
        },
    );
}
