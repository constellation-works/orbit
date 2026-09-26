use std::fs;

use orbit_common::OrbitError;
use orbit_common::governance::friction::FrictionVerb;
use orbit_tools::OrbitBuiltinAction;
use serde_json::json;

use super::super::artifact_redaction::{artifact_target, sanitize_tool_input};
use super::super::test_support::test_runtime;

/// Set one variable under the process-wide env guard shared by every
/// env-mutating test in this binary; restored on drop.
fn env_var(name: &'static str, value: &str) -> orbit_common::test_env::ScopedEnv {
    orbit_common::test_env::scoped([(name, Some(value))])
}

#[test]
fn sanitizer_covers_task_free_text_paths_and_skipped_tags() {
    let home = std::env::var("HOME").expect("HOME for redaction test");
    let input = json!({
        "title": "uses sk-abcdefghijklmnopqrstuvwxyz",
        "description": "plain",
        "acceptance_criteria": ["keep ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZabcd123456"],
        "context_files": [format!("{home}/repo/src/lib.rs"), "glob/[sk-abcdefghijklmnopqrstuvwxyz].rs"],
        "tags": ["sk-abcdefghijklmnopqrstuvwxyz"],
    });

    let (sanitized, report) =
        sanitize_tool_input(OrbitBuiltinAction::TaskAdd, input).expect("sanitize");

    assert!(report.redactions_applied());
    assert_eq!(sanitized["title"], "uses [REDACTED_SECRET]");
    assert!(
        sanitized["acceptance_criteria"][0]
            .as_str()
            .expect("criterion")
            .contains("[REDACTED_SECRET]")
    );
    assert_eq!(sanitized["context_files"][0], "~/repo/src/lib.rs");
    assert_eq!(
        sanitized["context_files"][1],
        "glob/[sk-abcdefghijklmnopqrstuvwxyz].rs"
    );
    assert_eq!(sanitized["tags"][0], "sk-abcdefghijklmnopqrstuvwxyz");
}

#[test]
fn whole_token_credentials_are_rejected_for_representative_free_text_surfaces() {
    let cases = [
        (
            OrbitBuiltinAction::AdrAdd,
            json!({
                "title": "sk-abcdefghijklmnopqrstuvwxyz",
                "body": "Body",
            }),
        ),
        (
            OrbitBuiltinAction::TaskAdd,
            json!({
                "title": "xoxb-0123456789",
                "description": "Body",
                "workspace": ".",
            }),
        ),
        (
            OrbitBuiltinAction::Friction(FrictionVerb::Add),
            json!({
                "body": "ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZabcd123456",
            }),
        ),
    ];

    for (action, input) in cases {
        let err = sanitize_tool_input(action, input).expect_err("whole-token key rejected");

        assert!(
            matches!(err, OrbitError::SensitiveInput { .. }),
            "{action:?}: {err:?}"
        );
    }
}

#[test]
fn sanitizer_covers_auto_task_definition_and_template_free_text() {
    let input = json!({
        "description": "definition sk-abcdefghijklmnopqrstuvwxyz",
        "template": {
            "title": "title sk-abcdefghijklmnopqrstuvwxyz",
            "description": "description sk-abcdefghijklmnopqrstuvwxyz",
            "acceptance_criteria": ["criterion sk-abcdefghijklmnopqrstuvwxyz"],
        },
    });

    let (sanitized, report) =
        sanitize_tool_input(OrbitBuiltinAction::AutoTaskAdd, input).expect("sanitize");

    assert!(report.redactions_applied());
    assert_eq!(sanitized["description"], "definition [REDACTED_SECRET]");
    assert_eq!(sanitized["template"]["title"], "title [REDACTED_SECRET]");
    assert_eq!(
        sanitized["template"]["description"],
        "description [REDACTED_SECRET]"
    );
    assert_eq!(
        sanitized["template"]["acceptance_criteria"][0],
        "criterion [REDACTED_SECRET]"
    );
}

#[test]
fn already_sanitized_input_is_idempotent() {
    let input = json!({
        "id": "ORB-00001",
        "execution_summary": "token [REDACTED_ENV]",
    });

    let (_sanitized, report) =
        sanitize_tool_input(OrbitBuiltinAction::TaskUpdate, input).expect("sanitize");

    assert!(!report.redactions_applied());
}

#[test]
fn wrong_types_pass_through_to_existing_parsers() {
    let input = json!({
        "title": ["not", "a", "string"],
        "body": "Body",
    });

    let (sanitized, report) =
        sanitize_tool_input(OrbitBuiltinAction::AdrAdd, input.clone()).expect("sanitize");

    assert_eq!(sanitized, input);
    assert!(!report.redactions_applied());
}

#[test]
fn dispatch_preserves_common_word_github_token_in_task_description() {
    let word = "user";
    let _env = env_var("GITHUB_TOKEN", word);
    let (_root, runtime, _repo_root) = test_runtime();
    let description = format!("No {word}-facing CLI behavior should change.");

    let output = runtime
        .execute_tool_command(
            "orbit.task.add",
            json!({
                "title": "plain",
                "description": description,
                "complexity": "low",
                "workspace": ".",
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("task add succeeds");

    assert_eq!(output["redactions_applied"], false);
    assert_eq!(output["description"], description);
    let id = output["id"].as_str().expect("task id");
    let task = runtime.get_task(id).expect("task persisted");
    assert_eq!(task.description, description);
}

#[test]
fn dispatch_redacts_live_github_token_before_task_persistence_and_audits() {
    let token = "orbit-redaction-secret-value";
    let _env = env_var("GITHUB_TOKEN", token);
    let (_root, runtime, _repo_root) = test_runtime();

    let output = runtime
        .execute_tool_command(
            "orbit.task.add",
            json!({
                "title": format!("leaked {token}"),
                "description": "body",
                "complexity": "low",
                "workspace": ".",
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("task add succeeds");

    assert_eq!(output["redactions_applied"], true);
    assert_eq!(
        output["redactions"],
        json!([{
            "field_path": "title",
            "redaction_kinds": ["env"],
            "redaction_classes": ["sensitive_environment_value"]
        }])
    );
    let id = output["id"].as_str().expect("task id");
    let task = runtime.get_task(id).expect("task persisted");
    assert_eq!(task.title, "leaked [REDACTED_ENV]");
    assert!(!task.title.contains(token));

    let events = runtime
        .list_audit_events(None, Some("orbit.task.add".to_string()), None, None, 16)
        .expect("L-0009: same backing query as `orbit audit list --json`");
    let redaction_event = events
        .iter()
        .find(|event| event.command == "artifact_redaction")
        .expect("redaction audit event");
    let arguments = redaction_event
        .arguments_json
        .as_deref()
        .expect("redaction audit payload");
    assert!(arguments.contains("\"field_path\":\"title\""));
    assert!(arguments.contains("\"env\""));
    assert!(!arguments.contains(token));
}

#[test]
fn projected_task_add_id_redacts_once_and_audits_persisted_id_without_secret_payload() {
    let token = "orbit-projected-add-secret-value";
    let _env = env_var("GITHUB_TOKEN", token);
    let (_root, runtime, _repo_root) = test_runtime();

    let output = runtime
        .execute_tool_command(
            "orbit.task.add",
            json!({
                "title": "Projected add",
                "description": format!("contains {token}"),
                "complexity": "low",
                "workspace": ".",
                "fields": ["id"],
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("projected task add succeeds after redaction");
    let id = output.as_str().expect("single id projection is a string");
    let tasks = runtime.list_tasks().expect("list persisted tasks");
    assert_eq!(tasks.len(), 1, "a retry would create a duplicate task");
    assert_eq!(tasks[0].id, id);
    assert_eq!(tasks[0].description, "contains [REDACTED_ENV]");

    let events = runtime
        .list_audit_events(None, Some("orbit.task.add".to_string()), None, None, 16)
        .expect("audit query");
    let audit = events
        .iter()
        .find(|event| event.command == "artifact_redaction")
        .expect("redaction audit event");
    assert_eq!(audit.target_id.as_deref(), Some(id));
    assert_eq!(audit.task_id.as_deref(), Some(id));
    let payload = audit.arguments_json.as_deref().expect("audit payload");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(payload).expect("json")["artifact_id"],
        id
    );
    assert!(
        !payload.contains(token),
        "audit payload must not contain secret material"
    );
}

#[test]
fn projected_task_update_without_id_redacts_stored_summary_and_audits_id() {
    let token = "orbit-projected-update-secret-value";
    let _env = env_var("GITHUB_TOKEN", token);
    let (_root, runtime, _repo_root) = test_runtime();
    let created = runtime
        .execute_tool_command(
            "orbit.task.add",
            json!({"title": "Projected update", "description": "body", "complexity": "low", "workspace": "."}),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("create task");
    let id = created["id"].as_str().expect("task id");

    let output = runtime
        .execute_tool_command(
            "orbit.task.update",
            json!({"id": id, "execution_summary": format!("summary {token}"), "fields": ["execution_summary", "status"]}),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("projected task update succeeds after redaction");
    assert!(output.get("id").is_none());
    assert_eq!(output["execution_summary"], "summary [REDACTED_ENV]");
    assert_eq!(output["redactions_applied"], true);
    assert_eq!(
        runtime
            .get_task(id)
            .expect("persisted task")
            .execution_summary,
        "summary [REDACTED_ENV]"
    );

    let events = runtime
        .list_audit_events(None, Some("orbit.task.update".to_string()), None, None, 16)
        .expect("audit query");
    let audit = events
        .iter()
        .find(|event| event.command == "artifact_redaction")
        .expect("redaction audit event");
    assert_eq!(audit.target_id.as_deref(), Some(id));
    assert_eq!(audit.task_id.as_deref(), Some(id));
    assert!(
        !audit
            .arguments_json
            .as_deref()
            .expect("audit payload")
            .contains(token)
    );
}

#[test]
fn redacted_task_add_validation_failure_creates_no_record() {
    let token = "orbit-invalid-add-secret-value";
    let _env = env_var("GITHUB_TOKEN", token);
    let (_root, runtime, _repo_root) = test_runtime();
    let error = runtime
        .execute_tool_command(
            "orbit.task.add",
            json!({"title": "Invalid", "description": format!("contains {token}"), "complexity": "not-a-complexity", "workspace": ".", "fields": ["id"]}),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect_err("invalid complexity must fail before persistence");
    assert!(matches!(error, OrbitError::InvalidInput(_)), "{error:?}");
    assert!(runtime.list_tasks().expect("list tasks").is_empty());
}

#[test]
fn redacted_task_update_missing_record_fails_without_creating_one() {
    let token = "orbit-missing-update-secret-value";
    let _env = env_var("GITHUB_TOKEN", token);
    let (_root, runtime, _repo_root) = test_runtime();
    let error = runtime
        .execute_tool_command(
            "orbit.task.update",
            json!({"id": "ORB-99999999", "execution_summary": format!("contains {token}"), "fields": ["execution_summary", "status"]}),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect_err("missing task must fail before audit");
    assert!(matches!(error, OrbitError::NotFound { .. }), "{error:?}");
    assert!(runtime.list_tasks().expect("list tasks").is_empty());
}

#[test]
fn whole_token_task_add_refuses_before_persistence() {
    let (_root, runtime, _repo_root) = test_runtime();
    let error = runtime
        .execute_tool_command(
            "orbit.task.add",
            json!({"title": "Whole token", "description": "sk-abcdefghijklmnopqrstuvwxyz", "complexity": "low", "workspace": ".", "fields": ["id"]}),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect_err("whole-token credential must be refused");
    assert!(
        matches!(error, OrbitError::SensitiveInput { .. }),
        "{error:?}"
    );
    assert!(runtime.list_tasks().expect("list tasks").is_empty());
}

#[test]
fn unsupported_redaction_audit_action_still_fails() {
    let response = json!({"id": "ORB-12345"});
    let error = artifact_target(OrbitBuiltinAction::TaskDelete, &response, None)
        .expect_err("unsupported audit action must fail");
    assert!(matches!(error, OrbitError::Execution(_)), "{error:?}");
}

#[test]
fn redaction_audit_missing_persisted_task_attribution_still_fails() {
    let response = json!({"status": "proposed"});
    let error = artifact_target(OrbitBuiltinAction::TaskAdd, &response, None)
        .expect_err("missing persisted id must fail");
    assert!(matches!(error, OrbitError::Execution(_)), "{error:?}");
}

#[test]
fn dispatch_reports_structural_ssh_redaction_classes() {
    let (_root, runtime, _repo_root) = test_runtime();
    let fingerprint = format!("SHA256:{}", "A".repeat(43));

    let output = runtime
        .execute_tool_command(
            "orbit.task.add",
            json!({
                "title": "SSH diagnostic",
                "description": format!(
                    "debug1: Connecting to build-node.example.test [192.0.2.10] port 22.\n256 {fingerprint} automation@build-node.example.test (ED25519)"
                ),
                "complexity": "low",
                "workspace": ".",
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("task add succeeds");

    assert_eq!(output["redactions_applied"], true);
    assert_eq!(
        output["redactions"],
        json!([{
            "field_path": "description",
            "redaction_kinds": ["pattern"],
            "redaction_classes": ["ssh_fingerprint", "ssh_host", "ssh_key_comment"]
        }])
    );
}

#[test]
fn dispatch_marks_false_and_emits_no_audit_when_input_is_already_sanitized() {
    let (_root, runtime, _repo_root) = test_runtime();
    let created = runtime
        .execute_tool_command(
            "orbit.task.add",
            json!({
                "title": "plain",
                "description": "body",
                "complexity": "low",
                "workspace": ".",
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("task add succeeds");
    let id = created["id"].as_str().expect("task id");

    let output = runtime
        .execute_tool_command(
            "orbit.task.update",
            json!({
                "id": id,
                "execution_summary": "already [REDACTED_ENV]",
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("task update succeeds");

    assert_eq!(output["redactions_applied"], false);
    assert_eq!(output["redactions"], json!([]));
    let events = runtime
        .list_audit_events(None, Some("orbit.task.update".to_string()), None, None, 16)
        .expect("L-0009: same backing query as `orbit audit list --json`");
    assert!(
        events
            .iter()
            .all(|event| event.command != "artifact_redaction"),
        "{events:?}"
    );
}

#[test]
fn dispatch_adds_false_response_flags_for_each_covered_family() {
    let (_root, runtime, _repo_root) = test_runtime();

    let task = runtime
        .execute_tool_command(
            "orbit.task.add",
            json!({
                "title": "plain",
                "description": "body",
                "complexity": "low",
                "workspace": ".",
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("task add succeeds");
    assert_eq!(task["redactions_applied"], false);

    let friction = runtime
        .execute_tool_command(
            "orbit.friction.add",
            json!({
                "body": "Plain friction report.",
                "tags": ["tooling"],
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("friction add succeeds");
    assert_eq!(friction["redactions_applied"], false);
}

#[test]
fn dispatch_round_trips_identifiers_and_reports_synthetic_provider_key_redaction() {
    let (_root, runtime, _repo_root) = test_runtime();
    let identifier = "remove-task-checkout-projections";
    let key = "sk-abcdefghijklmnopqrstuvwxyz";

    let task = runtime
        .execute_tool_command(
            "orbit.task.add",
            json!({
                "title": identifier,
                "description": format!("migration {identifier}; credential {key}"),
                "complexity": "low",
                "workspace": ".",
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("task add succeeds");

    assert_eq!(task["redactions_applied"], true);
    assert_eq!(task["title"], identifier);
    assert_eq!(
        task["description"],
        format!("migration {identifier}; credential [REDACTED_SECRET]")
    );
    assert_eq!(task["redactions"][0]["field_path"], "description");
    assert_eq!(task["redactions"][0]["redaction_kinds"], json!(["pattern"]));
    assert_eq!(
        task["redactions"][0]["redaction_classes"],
        json!(["credential"])
    );
    let task_id = task["id"].as_str().expect("task id");
    assert_eq!(
        runtime.get_task(task_id).expect("task persisted").title,
        identifier
    );

    let friction = runtime
        .execute_tool_command(
            "orbit.friction.add",
            json!({
                "body": format!("migration {identifier}; credential {key}"),
                "tags": ["tooling"],
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("friction add succeeds");

    assert_eq!(friction["redactions_applied"], true);
    assert_eq!(
        friction["body"],
        format!("migration {identifier}; credential [REDACTED_SECRET]")
    );
    assert_eq!(friction["redactions"][0]["field_path"], "body");
    assert_eq!(
        friction["redactions"][0]["redaction_kinds"],
        json!(["pattern"])
    );
    assert_eq!(
        friction["redactions"][0]["redaction_classes"],
        json!(["credential"])
    );
    let shown = runtime
        .run_tool("orbit.friction.show", json!({ "id": friction["id"] }))
        .expect("friction show succeeds");
    assert_eq!(
        shown["body"],
        format!("migration {identifier}; credential [REDACTED_SECRET]")
    );
}

#[test]
fn friction_body_update_is_sanitized_but_tags_are_verbatim() {
    let token = "orbit-friction-secret-value";
    let _env = env_var("GITHUB_TOKEN", token);
    let (_root, runtime, _repo_root) = test_runtime();
    let tag = "sk-abcdefghijklmnopqrstuvwx";
    let frictions_root = runtime.data_root().join("frictions");
    fs::create_dir_all(&frictions_root).expect("frictions root");
    fs::write(
        frictions_root.join("tags.yaml"),
        format!("{tag}: \"synthetic test tag\"\n"),
    )
    .expect("custom friction taxonomy");
    let created = runtime
        .execute_tool_command(
            "orbit.friction.add",
            json!({
                "body": "Plain friction report.",
                "tags": [tag],
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("friction add succeeds");
    assert_eq!(created["tags"], json!([tag]));

    let updated = runtime
        .execute_tool_command(
            "orbit.friction.update",
            json!({
                "id": created["id"],
                "body": format!("updated {token}"),
                "tags": [tag],
            }),
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
        )
        .expect("friction update succeeds");

    assert_eq!(updated["redactions_applied"], true);
    assert_eq!(updated["body"], "updated [REDACTED_ENV]");
    assert_eq!(updated["tags"], json!([tag]));
    assert!(
        !updated["body"].as_str().expect("body").contains(token),
        "{}",
        updated
    );
    let events = runtime
        .list_audit_events(
            None,
            Some("orbit.friction.update".to_string()),
            None,
            None,
            16,
        )
        .expect("audit query");
    let audit = events
        .iter()
        .find(|event| event.command == "artifact_redaction")
        .expect("friction redaction audit event");
    assert_eq!(audit.target_id.as_deref(), updated["id"].as_str());
    assert!(
        !audit
            .arguments_json
            .as_deref()
            .expect("audit payload")
            .contains(token)
    );
}
