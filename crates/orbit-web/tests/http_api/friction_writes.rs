//! Dashboard friction writes reach the friction tools' input check, as the
//! CLI and MCP do: a field the tool does not declare is reported, not dropped
//! on the way.

use serde_json::{Value, json};

use super::support::{Fixture, isolated, json_ok};

const WORKSPACE: &str = "?workspace=ws_http_fixture";

/// The unknown-field audit rows written for `tool`, as `(field, tool)` pairs.
fn unknown_field_rows(fixture: &Fixture, tool: &str) -> Vec<(String, String)> {
    fixture
        .runtime
        .list_audit_events(None, Some(tool.to_string()), None, None, 100)
        .expect("list audit events")
        .into_iter()
        .filter(|event| event.command == "tool_input")
        .map(|event| {
            let arguments: Value =
                serde_json::from_str(event.arguments_json.as_deref().expect("audit arguments"))
                    .expect("audit arguments JSON");
            (
                arguments["field"].as_str().expect("field").to_string(),
                arguments["tool_name"].as_str().expect("tool").to_string(),
            )
        })
        .collect()
}

#[test]
fn dashboard_friction_writes_forward_undeclared_fields_to_the_tool_entry() {
    isolated(
        "friction_writes::dashboard_friction_writes_forward_undeclared_fields_to_the_tool_entry",
        || {
            let fixture = Fixture::new();
            let server = fixture.server(true);

            let created = json_ok(server.send(
                "POST",
                &format!("/api/frictions{WORKSPACE}"),
                json!({"body": "Dashboard friction with an extra field", "severity": "high"}),
            ));
            let id = created["id"].as_str().expect("friction id");
            assert_eq!(
                unknown_field_rows(&fixture, "orbit.friction.add"),
                [("severity".to_string(), "orbit.friction.add".to_string())]
            );

            let updated = json_ok(server.send(
                "PATCH",
                &format!("/api/frictions/{id}{WORKSPACE}"),
                json!({"status": "triaged", "priority": 1}),
            ));
            assert_eq!(updated["status"], "triaged");
            assert_eq!(
                unknown_field_rows(&fixture, "orbit.friction.update"),
                [("priority".to_string(), "orbit.friction.update".to_string())]
            );
        },
    );
}
