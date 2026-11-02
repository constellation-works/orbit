//! Payload-bound auto-task callbacks through the private public-process fixture.
use super::Fixture;
use super::application_authority::{app_command, consent};
use chrono::{Duration, Utc};
use serde_json::{Value, json};

fn artifact_field(field: &str) -> Value {
    // Exercise RFC 6901 escaping on a real artifact, including object/array values.
    json!({"artifact_json": {"path": "proposal.json", "pointer": format!("/auto~1task~0proposal/{field}")}})
}

fn proposal(fixture: &Fixture, payload: &Value) -> (String, String) {
    let task = fixture.json([
        "task",
        "add",
        "--title",
        "Private auto-task proposal",
        "--complexity",
        "low",
        "--description",
        "Apply the reviewed payload",
        "--tag",
        "proposal",
    ]);
    let id = task["id"].as_str().unwrap().to_owned();
    assert_eq!(task["status"], "proposed");
    let source = fixture.work.join(".orbit/tmp/proposal.json");
    std::fs::create_dir_all(source.parent().unwrap()).unwrap();
    let bytes = serde_json::to_vec(payload).unwrap();
    std::fs::write(&source, &bytes).unwrap();
    fixture
        .operator_command([
            "tool",
            "run",
            "orbit.task.artifact.put",
            "--input",
            &json!({"id": id, "path": "proposal.json", "source_path": source}).to_string(),
        ])
        .assert()
        .success();
    (id, orbit_common::security::release::sha256_hex(&bytes))
}

fn callback_plugin(fixture: &Fixture, script: &str) -> Value {
    let source = fixture.source("autoproposal");
    let root = super::write_shell_backend(&source, script);
    std::fs::write(
        root.join("plugin.yaml"),
        r#"schemaVersion: 2
kind: Plugin
metadata:
  name: autoproposal
  version: 0.1.0
  description: Apply one reviewed private auto-task proposal.
spec:
  permissions:
    orbit_tools: [orbit.auto_task.add, orbit.auto_task.update, orbit.auto_task.mint]
  backend:
    type: exec
    command: bin/backend.sh
  tools:
    - name: apply
      description: Apply a reviewed payload.
      execution_kind: mutating
      mcp_scope: workspace
      input_schema:
        type: object
        properties:
          id: { type: string, description: Proposal task id. }
          sha256: { type: string, description: Digest of the proposal bytes. }
"#,
    )
    .unwrap();
    fixture.add(&source, &["--enable", "--grant", "orbit_tools"]);
    let shown = fixture.json(["plugin", "show", "autoproposal"]);
    json!({"name": "autoproposal", "manifest_digest": shown["manifest_digest"]})
}

fn request(plugin: Value, callbacks: Value) -> Value {
    json!({
        "application_id": "auto-task-app",
        "expires_at": (Utc::now() + Duration::days(7)).to_rfc3339(),
        "scopes": [{
            "name": "auto-task-proposal",
            "operation": {"tool": "autoproposal.apply", "plugin": plugin, "fields": {
                "id": {"task": {"tags_all": ["proposal"], "status": ["proposed"],
                    "artifact_sha256": {"path": "proposal.json", "field": "sha256"}}},
                "sha256": {"sha256": {}}
            }},
            "callbacks": callbacks
        }]
    })
}

fn audit_rows(fixture: &Fixture, tool: &str) -> Vec<(String, Value)> {
    let conn = rusqlite::Connection::open(fixture.home.join(".orbit/orbit.db")).unwrap();
    conn.prepare("SELECT status, application_json FROM audit_events WHERE tool_name = ?1 AND application_json IS NOT NULL")
        .unwrap().query_map([tool], |row| {
            let raw: String = row.get(1)?;
            Ok((row.get(0)?, serde_json::from_str(&raw).unwrap()))
        }).unwrap().map(Result::unwrap).collect()
}

#[test]
fn application_auto_task_add_is_bound_to_the_artifact_and_audited() {
    if !orbit_exec::macos_sandbox_test_guard(
        "application_auto_task_add_is_bound_to_the_artifact_and_audited",
    ) {
        return;
    }
    let fixture = Fixture::new();
    let approved = json!({
        "name": "approved-private-task", "description": "Reviewed recurring task",
        "schedule": {"every_minutes": 60},
        "template": {"title": "Private recurring task", "description": "Only in this fixture",
            "acceptance_criteria": ["Check private state"], "complexity": "low", "tags": ["private"]},
        "dedupe": "skip_if_open"
    });
    let (id, digest) = proposal(&fixture, &json!({"auto/task~proposal": approved}));
    let mut wrong_name = approved.clone();
    wrong_name["name"] = json!("unapproved-private-task");
    let mut wrong_schedule = approved.clone();
    wrong_schedule["schedule"] = json!({"every_minutes": 1});
    let script = format!(
        r#"#!/bin/sh
cat >/dev/null
run() {{ orbit tool run "$1" --input "$2" --format json; }}
if run orbit.auto_task.add '{wrong_name}' >/dev/null 2>&1; then exit 71; fi
if run orbit.auto_task.add '{wrong_schedule}' >/dev/null 2>&1; then exit 72; fi
run orbit.auto_task.add '{approved}' >/dev/null || exit 73
if run orbit.auto_task.mint '{{"name":"approved-private-task"}}' >/dev/null 2>&1; then exit 74; fi
printf '{{"ok":true,"output":{{"applied":true}}}}\n'
"#
    );
    let plugin = callback_plugin(&fixture, &script);
    let mut fields = serde_json::Map::new();
    for field in approved.as_object().unwrap().keys() {
        fields.insert(field.clone(), artifact_field(field));
    }
    let grant_request = request(
        plugin,
        json!([{"tool": "orbit.auto_task.add", "fields": fields}]),
    );
    let (grant, credential) = consent(&fixture, &grant_request, "auto-add", None);
    let entry = json!({"id": id, "sha256": digest});
    app_command(&fixture, &credential, "autoproposal.apply", &entry)
        .env("ORBIT_AGENT_NAME", "codex")
        .assert()
        .failure();
    assert!(
        !fixture
            .work
            .join(".orbit/auto_tasks/approved-private-task.yaml")
            .exists()
    );
    app_command(&fixture, &credential, "autoproposal.apply", &entry)
        .assert()
        .success();
    let actual = fixture.json(["auto-task", "show", "approved-private-task"]);
    for field in approved.as_object().unwrap().keys() {
        // Definition templates hydrate defaults, so compare their supplied values below.
        if field != "template" {
            assert_eq!(actual[field], approved[field], "{field}");
        }
    }
    for (key, value) in approved["template"].as_object().unwrap() {
        assert_eq!(&actual["template"][key], value, "template {key}");
    }
    assert_eq!(actual["created_by"], "application:auto-task-app");
    assert!(
        !fixture
            .work
            .join(".orbit/auto_tasks/unapproved-private-task.yaml")
            .exists()
    );
    let rows = audit_rows(&fixture, "orbit.auto_task.add");
    assert!(
        rows.iter().any(|(status, app)| status == "success"
            && app["callback"] == true
            && app["consent"]["consent_id"] == grant["consent_id"]
            && app["predicate"]["artifact"]["sha256"] == digest
            && app["bindings"]["id"] == id
            && app["match"] == "constrained"),
        "{rows:?}"
    );
    assert_eq!(
        rows.iter().filter(|(status, _)| status == "denied").count(),
        2,
        "{rows:?}"
    );
}

#[test]
fn application_auto_task_update_refuses_inputs_outside_the_approved_ceiling() {
    if !orbit_exec::macos_sandbox_test_guard(
        "application_auto_task_update_refuses_inputs_outside_the_approved_ceiling",
    ) {
        return;
    }
    let fixture = Fixture::new();
    for name in ["approved-private-task", "other-private-task"] {
        fixture
            .operator_command([
                "auto-task",
                "add",
                "--name",
                name,
                "--every-minutes",
                "60",
                "--title",
                "Private recurring task",
                "--complexity",
                "low",
            ])
            .assert()
            .success();
    }
    let approved = json!({"name": "approved-private-task", "enabled": false});
    let (id, digest) = proposal(&fixture, &json!({"auto/task~proposal": approved}));
    let script = format!(
        r#"#!/bin/sh
cat >/dev/null
run() {{ orbit tool run orbit.auto_task.update --input "$1" --format json; }}
if run '{{"name":"other-private-task","enabled":false}}' >/dev/null 2>&1; then exit 71; fi
if run '{{"name":"approved-private-task","enabled":true}}' >/dev/null 2>&1; then exit 72; fi
if run '{{"name":"approved-private-task","enabled":false,"expected_enabled":true}}' >/dev/null 2>&1; then exit 73; fi
if run '{{"name":"approved-private-task","enabled":false,"schedule":{{"every_minutes":1}}}}' >/dev/null 2>&1; then exit 74; fi
run '{approved}' >/dev/null || exit 75
printf '{{"ok":true,"output":{{"applied":true}}}}\n'
"#
    );
    let plugin = callback_plugin(&fixture, &script);
    let grant_request = request(
        plugin,
        json!([{"tool": "orbit.auto_task.update", "fields": {
            "name": artifact_field("name"), "enabled": {"const": false}
        }}]),
    );
    let (_, credential) = consent(&fixture, &grant_request, "auto-update", None);
    app_command(
        &fixture,
        &credential,
        "autoproposal.apply",
        &json!({"id": id, "sha256": digest}),
    )
    .assert()
    .success();
    let actual = fixture.json(["auto-task", "show", "approved-private-task"]);
    assert_eq!(actual["enabled"], false);
    assert_eq!(actual["schedule"]["every_minutes"], 60);
    assert_eq!(actual["updated_by"], "application:auto-task-app");
    assert_eq!(
        fixture.json(["auto-task", "show", "other-private-task"])["enabled"],
        true
    );
    let rows = audit_rows(&fixture, "orbit.auto_task.update");
    assert!(
        rows.iter()
            .any(|(status, app)| status == "success" && app["callback"] == true),
        "{rows:?}"
    );
    assert_eq!(
        rows.iter().filter(|(status, _)| status == "denied").count(),
        4,
        "{rows:?}"
    );
    // A consent cannot smuggle the Operator-only input into its ceiling either.
    let mut refused = grant_request.clone();
    refused["scopes"][0]["callbacks"][0]["fields"]["expected_enabled"] = json!({"const": true});
    let source = fixture.home.join("refused-consent.json");
    std::fs::write(&source, refused.to_string()).unwrap();
    fixture
        .operator_command([
            "application",
            "consent",
            "--input-file",
            source.to_str().unwrap(),
            "--credential-file",
            fixture.home.join("refused-credential").to_str().unwrap(),
        ])
        .assert()
        .failure();
}
