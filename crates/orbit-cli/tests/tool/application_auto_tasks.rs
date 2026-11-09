//! Payload-bound auto-task callbacks: a consented fixture plugin creates and
//! updates a private definition only when the call matches the entry-bound
//! artifact, and refuses everything outside that ceiling.
use super::Fixture;
use super::application_authority::{app_command, consent};
use chrono::{Duration, SecondsFormat, Utc};
use serde_json::{Value, json};

fn expires_in(days: i64) -> String {
    (Utc::now() + Duration::days(days)).to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn sha256_hex(bytes: &[u8]) -> String {
    orbit_common::security::release::sha256_hex(bytes)
}

fn proposal_task(fixture: &Fixture, payload: &str) -> String {
    let task = fixture.json([
        "task",
        "add",
        "--title",
        "Office auto-task proposal",
        "--complexity",
        "low",
        "--description",
        "Application callback target.",
        "--tag",
        "proposal",
    ]);
    let id = task["id"].as_str().unwrap().to_owned();
    if task["status"] != "proposed" {
        fixture
            .operator_command(["task", "update", &id, "--status", "proposed", "--force"])
            .assert()
            .success();
    }
    let source = fixture.work.join(format!("proposal-{id}.json"));
    std::fs::write(&source, payload).unwrap();
    let input = json!({"action": "put", "id": id, "path": "proposal.json", "source_path": source});
    fixture
        .operator_command([
            "tool",
            "run",
            "orbit.task.artifact",
            "--input",
            &input.to_string(),
        ])
        .assert()
        .success();
    std::fs::remove_file(source).unwrap();
    id
}

fn install_office_plugin(fixture: &Fixture, script: &str) -> Value {
    let source = fixture.source("office");
    let root = super::write_shell_backend(&source, script);
    std::fs::write(
        root.join("plugin.yaml"),
        r#"schemaVersion: 2
kind: Plugin
metadata:
  name: office
  version: 0.1.0
  description: Accepts one reviewed auto-task proposal.
spec:
  permissions:
    orbit_tools: ["orbit.auto_task:add", "orbit.auto_task:update", orbit.task.update]
  backend:
    type: exec
    command: bin/backend.sh
  tools:
    - name: accept
      description: Accept a reviewed auto-task proposal.
      execution_kind: mutating
      mcp_scope: workspace
      input_schema:
        type: object
        properties:
          id: { type: string, description: Proposal task id. }
          sha256: { type: string, description: Digest of the proposal bytes. }
"#,
    )
    .expect("write manifest");
    fixture.add(&source, &["--enable", "--grant", "orbit_tools"]);
    let show = fixture.json(["plugin", "show", "office"]);
    json!({"name": "office", "manifest_digest": show["manifest_digest"]})
}

fn artifact_json(path: &str, pointer: &str) -> Value {
    json!({"artifact_json": {"path": path, "pointer": pointer}})
}

fn accept_scope(plugin: &Value) -> Value {
    json!({
        "name": "auto-task-accept",
        "operation": {
            "tool": "office.accept",
            "plugin": plugin,
            "fields": {
                "id": {"task": {"tags_all": ["proposal"], "status": ["proposed"],
                                "artifact_sha256": {"path": "proposal.json", "field": "sha256"}}},
                "sha256": {"sha256": {}}
            }
        },
        "callbacks": [
            {"tool": "orbit.auto_task.add", "plugin": null, "fields": {
                "name": artifact_json("proposal.json", "/add/name"),
                "description": artifact_json("proposal.json", "/add/description"),
                "schedule": artifact_json("proposal.json", "/add/schedule"),
                "template": artifact_json("proposal.json", "/add/template"),
                "dedupe": artifact_json("proposal.json", "/add/dedupe")
            }},
            {"tool": "orbit.auto_task.update", "plugin": null, "fields": {
                "name": artifact_json("proposal.json", "/update/name"),
                "enabled": {"const": false}
            }}
        ]
    })
}

fn consent_request(plugin: &Value) -> Value {
    json!({
        "application_id": "office-app",
        "expires_at": expires_in(7),
        "scopes": [accept_scope(plugin)],
    })
}

fn consent_fails(fixture: &Fixture, request: &Value, suffix: &str) {
    let input = fixture.home.join(format!("consent-{suffix}.json"));
    std::fs::write(&input, request.to_string()).unwrap();
    fixture
        .orbit_as_operator()
        .args(["application", "consent", "--input-file"])
        .arg(&input)
        .arg("--credential-file")
        .arg(fixture.home.join(format!("credential-{suffix}")))
        .assert()
        .failure();
}

fn application_rows(fixture: &Fixture, tool: &str) -> Vec<(String, Value)> {
    let conn = rusqlite::Connection::open(fixture.home.join(".orbit/orbit.db")).unwrap();
    conn.prepare(
        "SELECT status, application_json FROM audit_events \
         WHERE tool_name = ?1 AND application_json IS NOT NULL",
    )
    .unwrap()
    .query_map([tool], |row| {
        let raw: String = row.get(1)?;
        Ok((row.get(0)?, serde_json::from_str(&raw).unwrap()))
    })
    .unwrap()
    .map(Result::unwrap)
    .collect()
}

/// A consented add whose fields equal the digest-bound artifact is admitted
/// and stamped `application:<id>`. A different add, an update outside the
/// name/enabled ceiling, an unapproved callback and an agent identity are
/// refused without creating or changing another definition.
#[test]
fn application_auto_task_callbacks_follow_the_artifact_and_refuse_the_rest() {
    if !orbit_exec::macos_sandbox_test_guard(
        "application_auto_task_callbacks_follow_the_artifact_and_refuse_the_rest",
    ) {
        return;
    }
    let add = json!({
        "name": "office-chore",
        "description": "Created from the office proposal.",
        "schedule": {"every_minutes": 60},
        "template": {
            "title": "Office chore",
            "description": "Do the office chore.",
            "acceptance_criteria": ["Persisted"]
        },
        "dedupe": "skip_if_open"
    });
    let mut differing_add = add.clone();
    differing_add["name"] = json!("other-chore");
    let good_update = json!({"name": "office-chore", "enabled": false});
    let described_update = json!({
        "name": "office-chore",
        "enabled": false,
        "description": "changed outside the ceiling"
    });
    let floored_update = json!({"name": "office-chore", "expected_enabled": true});
    let unapproved = json!({"id": "not-a-task", "status": "done", "comment": "not consented"});
    let artifact = json!({
        "add": add,
        "update": {"name": "office-chore", "enabled": false}
    });
    let payload = serde_json::to_string(&artifact).unwrap();

    // The backend calls the `orbit.auto_task` family, naming each action.
    let family_call = |input: &Value, action: &str| {
        let mut call = input.clone();
        call["action"] = json!(action);
        call
    };
    let fixture = Fixture::new();
    let accepted = proposal_task(&fixture, &payload);
    let script = format!(
        r#"#!/bin/sh
cat >/dev/null
run() {{ orbit tool run "$1" --input "$2" --format json; }}
if run orbit.auto_task '{differing_add}' >/dev/null 2>&1; then exit 81; fi
run orbit.auto_task '{add}' >/dev/null || exit 82
if run orbit.auto_task '{described_update}' >/dev/null 2>&1; then exit 83; fi
if run orbit.auto_task '{floored_update}' >/dev/null 2>&1; then exit 84; fi
run orbit.auto_task '{good_update}' >/dev/null || exit 85
if run orbit.auto_task '{differing_add}' >/dev/null 2>&1; then exit 87; fi
if run orbit.task.update '{unapproved}' >/dev/null 2>&1; then exit 86; fi
printf '{{"ok":true,"output":{{"accepted":"auto-task-ok"}}}}\n'
"#,
        differing_add = family_call(&differing_add, "add"),
        add = family_call(&add, "add"),
        described_update = family_call(&described_update, "update"),
        floored_update = family_call(&floored_update, "update"),
        good_update = family_call(&good_update, "update"),
        unapproved = unapproved,
    );
    let plugin = install_office_plugin(&fixture, &script);

    let mut floored = consent_request(&plugin);
    floored["scopes"][0]["callbacks"][1]["fields"]["expected_enabled"] = json!({"const": true});
    consent_fails(&fixture, &floored, "floor");
    let mut exact = consent_request(&plugin);
    exact["scopes"][0]["callbacks"][0] = json!({
        "tool": "orbit.auto_task.add",
        "plugin": null,
        "input": {"name": "office-chore"}
    });
    consent_fails(&fixture, &exact, "exact");

    let (grant, credential) = consent(&fixture, &consent_request(&plugin), "office", None);
    assert!(grant["consent_id"].as_str().is_some());
    let entry = json!({"id": accepted, "sha256": sha256_hex(payload.as_bytes())});
    app_command(&fixture, &credential, "office.accept", &entry)
        .env("ORBIT_AGENT_NAME", "codex")
        .assert()
        .failure();
    fixture
        .command(["auto-task", "show", "office-chore"])
        .assert()
        .failure();

    let output = app_command(&fixture, &credential, "office.accept", &entry)
        .assert()
        .get_output()
        .clone();
    assert!(
        output.status.success(),
        "auto-task accept failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("auto-task-ok"));

    let shown = fixture.json(["auto-task", "show", "office-chore"]);
    assert_eq!(shown["name"], "office-chore");
    assert_eq!(shown["description"], "Created from the office proposal.");
    assert_eq!(shown["enabled"], false);
    assert_eq!(shown["created_by"], "application:office-app");
    assert_eq!(shown["updated_by"], "application:office-app");
    assert!(
        !fixture
            .work
            .join(".orbit/auto_tasks/other-chore.yaml")
            .exists(),
        "a differing add must not create a definition"
    );

    for tool in ["orbit.auto_task.add", "orbit.auto_task.update"] {
        let rows = application_rows(&fixture, tool);
        assert!(
            rows.iter().any(|(status, app)| status == "success"
                && app["callback"] == true
                && app["bindings"]["id"] == accepted.as_str()),
            "{tool} success: {rows:?}"
        );
        assert!(
            rows.iter()
                .any(|(status, app)| status == "denied" && app["callback"] == true),
            "{tool} denial: {rows:?}"
        );
    }
    assert!(
        application_rows(&fixture, "orbit.task.update")
            .iter()
            .any(|(status, app)| status == "denied" && app["callback"] == true),
        "unapproved callback was not refused"
    );
}
