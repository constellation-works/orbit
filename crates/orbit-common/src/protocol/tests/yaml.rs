use crate::OrbitError;
use crate::protocol::yaml::{parse_auto_task_yaml, parse_local_routine_yaml, parse_routine_yaml};
use orbit_types::workflow::{MissedRunPolicy, OverlapPolicy, RoutineTarget};

const VALID_ROUTINE: &str = r#"
schemaVersion: 1
name: almanac-auto-commit
description: Commit & push almanac changes nightly
enabled: true
hosts: [dk-mac]
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
    assert_eq!(routine.hosts, vec!["dk-mac".to_string()]);
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
hosts: [dk-server-1]
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
hosts: [dk-server-1]
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
             hosts: [dk-server-1]\n\
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
hosts: [dk-server-1]
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
hosts: [dk-server-1]
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
hosts: [dk-server-1]
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
fn rejects_empty_hosts_and_bad_names() {
    let no_hosts = parse_routine_yaml(
        r#"
schemaVersion: 1
name: reindex
hosts: []
trigger:
  cron: "*/30 * * * *"
target: job:docs_reindex
"#,
    )
    .expect_err("empty hosts must fail");
    assert!(
        no_hosts.to_string().contains("at least one host"),
        "{no_hosts}"
    );

    let bad_name = parse_routine_yaml(
        r#"
schemaVersion: 1
name: "Almanac Commit"
hosts: [dk-mac]
trigger:
  cron: "0 22 * * *"
target: job:almanac_commit_pipeline
"#,
    )
    .expect_err("uppercase/space name must fail");
    assert!(bad_name.to_string().contains("routine name"), "{bad_name}");
}

#[test]
fn local_routine_may_omit_hosts_and_pins_the_loading_host() {
    // A local definition carries no `hosts:` — it is implicit to the machine
    // loading it and must resolve without a registry or network.
    let routine = parse_local_routine_yaml(
        r#"
schemaVersion: 1
name: local-reindex
trigger:
  cron: "*/30 * * * *"
target: job:docs_reindex
"#,
        "dk-mac",
    )
    .expect("local routine loads without a host pin");
    // Normalized to an implicit single-host pin so downstream matching is
    // origin-agnostic.
    assert_eq!(routine.hosts, vec!["dk-mac".to_string()]);
}

#[test]
fn local_routine_may_name_only_the_loading_host() {
    let ok = parse_local_routine_yaml(
        r#"
schemaVersion: 1
name: local-reindex
hosts: [dk-mac]
trigger:
  cron: "*/30 * * * *"
target: job:docs_reindex
"#,
        "dk-mac",
    )
    .expect("local routine naming the loading host is valid");
    assert_eq!(ok.hosts, vec!["dk-mac".to_string()]);

    let remote = parse_local_routine_yaml(
        r#"
schemaVersion: 1
name: local-reindex
hosts: [dk-server-1]
trigger:
  cron: "*/30 * * * *"
target: job:docs_reindex
"#,
        "dk-mac",
    )
    .expect_err("a local definition naming another host is a remote pin and must fail");
    let message = remote.to_string();
    assert!(message.contains("local-reindex"), "{message}");
    assert!(message.contains("dk-server-1"), "{message}");
}

#[test]
fn target_round_trips_through_serde() {
    let routine = parse_routine_yaml(VALID_ROUTINE).expect("valid routine");
    let serialized = serde_yaml::to_string(&routine).expect("serialize");
    assert!(serialized.contains("target: job:almanac_commit_pipeline"));
    let reparsed = parse_routine_yaml(&serialized).expect("reparse");
    assert_eq!(reparsed, routine);
}
