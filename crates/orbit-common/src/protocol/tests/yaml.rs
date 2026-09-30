use crate::OrbitError;
use crate::protocol::yaml::{parse_auto_task_yaml, parse_policy_resource, parse_routine_yaml};
use orbit_types::workflow::{MissedRunPolicy, OverlapPolicy, RoutineTarget};

const VALID_ROUTINE: &str = r#"
schemaVersion: 1
name: almanac-auto-commit
description: Commit & push almanac changes nightly
enabled: true
trigger:
  cron: "0 22 * * *"
  missed_run: catch_up_once
target: job:almanac_commit_pipeline
policy:
  timeout_minutes: 10
  retries: { max: 2, backoff_minutes: 2 }
  overlap: forbid
"#;

const AUTO_TASK_PREFIX: &str = r#"
schemaVersion: 1
name: schedule-test
schedule:
"#;

const AUTO_TASK_TEMPLATE: &str = r#"
template:
  title: Schedule test
"#;

#[test]
fn auto_task_schedule_rejects_unknown_keys_with_accepted_forms() {
    let error = parse_auto_task_yaml(&format!(
        "{AUTO_TASK_PREFIX}  every_minute: 5\n{AUTO_TASK_TEMPLATE}"
    ))
    .expect_err("unknown schedule key must fail");
    let message = error.to_string();

    for expected in ["every_minute", "cron", "every_minutes", "deliveries_landed"] {
        assert!(message.contains(expected), "{message}");
    }
}

#[test]
fn auto_task_schedule_rejects_multiple_forms_with_readable_error() {
    let error = parse_auto_task_yaml(&format!(
        "{AUTO_TASK_PREFIX}  cron: \"* * * * *\"\n  every_minutes: 5\n{AUTO_TASK_TEMPLATE}"
    ))
    .expect_err("multiple schedule forms must fail");

    assert!(
        error
            .to_string()
            .contains("exactly one of cron, every_minutes, deliveries_landed"),
        "{error}"
    );
}

#[test]
fn auto_task_schedule_forms_round_trip_through_yaml() {
    let schedules = [
        "  cron: \"* * * * *\"\n",
        "  every_minutes: 5\n",
        concat!(
            "  deliveries_landed:\n",
            "    branch: agent-main\n",
            "    threshold: 1\n",
            "    max_wait_minutes: 60\n",
            "    coverage: landed_code_review_v1\n"
        ),
    ];

    for schedule in schedules {
        let definition =
            parse_auto_task_yaml(&format!("{AUTO_TASK_PREFIX}{schedule}{AUTO_TASK_TEMPLATE}"))
                .expect("schedule form must parse");
        let serialized = serde_yaml::to_string(&definition).expect("serialize schedule form");
        let reparsed = parse_auto_task_yaml(&serialized).expect("reparse schedule form");

        assert_eq!(reparsed, definition);
    }
}

#[test]
fn parses_the_design_doc_example() {
    let routine = parse_routine_yaml(VALID_ROUTINE).expect("valid routine");
    assert_eq!(routine.name, "almanac-auto-commit");
    assert!(routine.enabled);
    assert_eq!(routine.trigger.cron, "0 22 * * *");
    assert_eq!(routine.trigger.missed_run, MissedRunPolicy::CatchUpOnce);
    assert_eq!(
        routine.target,
        RoutineTarget::Job("almanac_commit_pipeline".to_string())
    );
    assert_eq!(routine.policy.timeout_minutes, 10);
    assert_eq!(routine.policy.retries.max, 2);
    assert_eq!(routine.policy.retries.backoff_minutes, 2);
    assert_eq!(routine.policy.overlap, OverlapPolicy::Forbid);
}

#[test]
fn defaults_apply_when_optional_fields_are_absent() {
    let routine = parse_routine_yaml(
        r#"
schemaVersion: 1
name: reindex
trigger:
  cron: "*/30 * * * *"
target: job:docs_reindex
"#,
    )
    .expect("valid minimal routine");
    assert!(routine.enabled);
    assert_eq!(routine.trigger.missed_run, MissedRunPolicy::Skip);
    assert_eq!(routine.policy.timeout_minutes, 60);
    assert_eq!(routine.policy.retries.max, 0);
    assert_eq!(routine.policy.overlap, OverlapPolicy::Forbid);
}

#[test]
fn rejects_unknown_fields_fail_closed() {
    let error = parse_routine_yaml(
        r#"
schemaVersion: 1
name: reindex
trigger:
  cron: "*/30 * * * *"
  jitter_seconds: 5
target: job:docs_reindex
"#,
    )
    .expect_err("unknown trigger field must fail");
    assert!(error.to_string().contains("jitter_seconds"), "{error}");
}

#[test]
fn rejects_duration_values_above_one_week() {
    for (field, policy) in [
        ("timeout_minutes", "timeout_minutes: 1000000000000000"),
        (
            "backoff_minutes",
            "retries: { max: 1, backoff_minutes: 1000000000000000 }",
        ),
    ] {
        let error = parse_routine_yaml(&format!(
            "schemaVersion: 1\n\
             name: reindex\n\
             trigger:\n  cron: \"*/30 * * * *\"\n\
             target: job:docs_reindex\n\
             policy:\n  {policy}\n"
        ))
        .expect_err("duration above one week must fail");

        assert!(
            matches!(error, OrbitError::InvalidInput(ref message) if message.contains(field)),
            "{error}"
        );
    }
}

#[test]
fn rejects_unsupported_schema_version() {
    let error = parse_routine_yaml(
        r#"
schemaVersion: 2
name: reindex
trigger:
  cron: "*/30 * * * *"
target: job:docs_reindex
"#,
    )
    .expect_err("schemaVersion 2 must fail");
    assert!(error.to_string().contains("schemaVersion 2"), "{error}");
}

#[test]
fn rejects_activity_targets_with_wrapping_guidance() {
    let error = parse_routine_yaml(
        r#"
schemaVersion: 1
name: reindex
trigger:
  cron: "*/30 * * * *"
target: activity:semantic_reindex
"#,
    )
    .expect_err("activity target must fail in v1");
    let message = error.to_string();
    assert!(message.contains("wrap the"), "{message}");
    assert!(message.contains("job:<name>"), "{message}");
}

#[test]
fn rejects_inline_command_shaped_targets() {
    let error = parse_routine_yaml(
        r#"
schemaVersion: 1
name: reindex
trigger:
  cron: "*/30 * * * *"
target: "sh -c 'rm -rf /'"
"#,
    )
    .expect_err("inline command target must fail");
    let message = error.to_string();
    assert!(message.contains("catalog reference"), "{message}");
    assert!(message.contains("job:<name>"), "{message}");
    assert!(
        message.contains("inline commands are not supported"),
        "{message}"
    );
}

#[test]
fn rejects_bad_names() {
    let bad_name = parse_routine_yaml(
        r#"
schemaVersion: 1
name: "Almanac Commit"
trigger:
  cron: "0 22 * * *"
target: job:almanac_commit_pipeline
"#,
    )
    .expect_err("uppercase/space name must fail");
    assert!(bad_name.to_string().contains("routine name"), "{bad_name}");
}

/// [ORB-12236] A definition written before host pins were retired still loads
/// — ignored, and flagged so the loader can name the file it came from.
#[test]
fn retired_hosts_key_loads_and_is_reported() {
    let routine = parse_routine_yaml(
        r#"
schemaVersion: 1
name: reindex
hosts: [some-other-host]
trigger:
  cron: "*/30 * * * *"
target: job:docs_reindex
"#,
    )
    .expect("a definition still carrying hosts: must load");
    assert!(routine.enabled);
    assert!(routine.has_legacy_host_pin());

    let without = parse_routine_yaml(
        r#"
schemaVersion: 1
name: reindex
trigger:
  cron: "*/30 * * * *"
target: job:docs_reindex
"#,
    )
    .expect("a definition without hosts: must load");
    assert!(!without.has_legacy_host_pin());
}

#[test]
fn target_round_trips_through_serde() {
    let routine = parse_routine_yaml(VALID_ROUTINE).expect("valid routine");
    let serialized = serde_yaml::to_string(&routine).expect("serialize");
    assert!(serialized.contains("target: job:almanac_commit_pipeline"));
    let reparsed = parse_routine_yaml(&serialized).expect("reparse");
    assert_eq!(reparsed, routine);
}

#[test]
fn routine_yaml_preserves_anchors_block_scalars_and_explicit_tags() {
    let routine = parse_routine_yaml(
        r#"---
schemaVersion: 1
name: &name yaml-compat
description: |-
  Unicode: café
  literal: on, off, null
enabled: !!bool true
trigger: { cron: "0 22 * * *", missed_run: skip }
target: job:yaml-compat
hosts: [*name]
"#,
    )
    .expect("legacy YAML forms load");
    assert_eq!(routine.description, "Unicode: café\nliteral: on, off, null");
    assert_eq!(
        routine.legacy_hosts.as_deref(),
        Some(std::slice::from_ref(&routine.name))
    );
    let serialized = serde_yaml::to_string(&routine).expect("serialize routine");
    let mut expected = routine;
    // Retired host pins intentionally disappear when the document is saved.
    expected.legacy_hosts = None;
    assert_eq!(parse_routine_yaml(&serialized).expect("reload"), expected);
}

#[test]
fn persisted_yaml_rejects_duplicate_fields_bad_syntax_and_document_streams() {
    for yaml in [
        VALID_ROUTINE.replace("enabled: true", "enabled: true\nenabled: false"),
        VALID_ROUTINE.replace("enabled: true", "enabled: ["),
        format!("{VALID_ROUTINE}\n---\n{VALID_ROUTINE}"),
        VALID_ROUTINE.replace("schemaVersion: 1", "schemaVersion: [1]"),
    ] {
        assert!(matches!(
            parse_routine_yaml(&yaml),
            Err(OrbitError::InvalidInput(_))
        ));
    }
    let error = parse_routine_yaml(&VALID_ROUTINE.replace("enabled: true", "enabled: ["))
        .expect_err("malformed YAML fails");
    let message = error.to_string();
    assert!(message.contains("routine header:"), "{message}");
    assert!(
        message.contains("line") && message.contains("column"),
        "{message}"
    );
}

#[test]
fn policy_yaml_round_trips_timestamps_patterns_and_quoted_scalars() {
    let yaml = r#"
schemaVersion: 2
kind: Policy
metadata: { name: yaml-compat }
spec:
  description: "null"
  created_at: 2026-09-08T12:00:00Z
  denyRead: ["**/.env", "secrets/**"]
  denyModify: ["**/.git/**"]
  fsProfiles:
    developer: { read: ["./**"], modify: ["src/**"] }
"#;
    let policy = parse_policy_resource(yaml, "policy fixture").expect("policy parses");
    assert_eq!(policy.spec.description.as_deref(), Some("null"));
    assert!(policy.spec.created_at.is_some());
    assert_eq!(policy.spec.deny_read, ["**/.env", "secrets/**"]);
    let serialized = serde_yaml::to_string(&policy).expect("serialize policy");
    assert_eq!(
        parse_policy_resource(&serialized, "policy fixture").expect("reload"),
        policy
    );
    for invalid in [
        yaml.replace("schemaVersion: 2", "schemaVersion: 1"),
        yaml.replace("kind: Policy", "kind: Job"),
        yaml.replace("2026-09-08T12:00:00Z", "not-a-timestamp"),
    ] {
        assert!(matches!(
            parse_policy_resource(&invalid, "policy fixture"),
            Err(OrbitError::InvalidInput(_))
        ));
    }
}
