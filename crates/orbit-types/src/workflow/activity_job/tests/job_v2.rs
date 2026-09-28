use super::super::job_v2::*;
use serde_json::{Value, json};

fn json_target_step() -> Value {
    json!({ "id": "guarded", "target": "activity:noop" })
}

fn json_target_step_with(field: &str, value: Value) -> Value {
    let mut step = json_target_step();
    step.as_object_mut()
        .expect("target step is an object")
        .insert(field.to_string(), value);
    step
}

fn assert_actionable_field_error(error: impl std::fmt::Display, field: &str) {
    let message = error.to_string();
    assert!(
        message.contains(field),
        "error should identify `{field}`: {message}"
    );
    assert!(
        message.contains("string"),
        "error should explain the expected type: {message}"
    );
}

#[test]
fn task_worktree_ownership_is_opt_in_and_round_trips() {
    let base = "state: enabled\nsteps: []\n";
    let ordinary: JobV2 = serde_yaml::from_str(base).expect("parse ordinary job");
    assert!(!ordinary.owns_task_worktree);
    assert!(
        !serde_json::to_value(&ordinary)
            .expect("serialize ordinary job")
            .as_object()
            .expect("job object")
            .contains_key("owns_task_worktree")
    );

    let delivery: JobV2 =
        serde_yaml::from_str("state: enabled\nowns_task_worktree: true\nsteps: []\n")
            .expect("parse delivery job");
    assert!(delivery.owns_task_worktree);
    let restored: JobV2 =
        serde_json::from_value(serde_json::to_value(delivery).expect("serialize delivery job"))
            .expect("deserialize delivery job");
    assert!(restored.owns_task_worktree);
}

#[test]
fn task_delivery_is_opt_in_and_declares_the_modes_a_task_may_select() {
    use crate::workflow::ShipMode;

    let ordinary: JobV2 = serde_yaml::from_str("state: enabled\nsteps: []\n").expect("parse");
    assert!(!ordinary.holds_task_delivery());
    assert!(!ordinary.delivers_mode(ShipMode::Local));

    let coordinator: JobV2 =
        serde_yaml::from_str("state: enabled\ntask_delivery: {}\nsteps: []\n").expect("parse");
    assert!(coordinator.holds_task_delivery());
    assert!(!coordinator.delivers_mode(ShipMode::Local));
    assert!(!coordinator.delivers_mode(ShipMode::Pr));

    let leaf: JobV2 =
        serde_yaml::from_str("state: enabled\ntask_delivery:\n  modes: [local]\nsteps: []\n")
            .expect("parse");
    assert!(leaf.delivers_mode(ShipMode::Local));
    assert!(!leaf.delivers_mode(ShipMode::Pr));
    let restored: JobV2 =
        serde_json::from_value(serde_json::to_value(&leaf).expect("serialize")).expect("restore");
    assert_eq!(restored.task_delivery, leaf.task_delivery);

    for invalid in [
        "state: enabled\ntask_delivery:\n  modes: [remote]\nsteps: []\n",
        "state: enabled\ntask_delivery:\n  mode: local\nsteps: []\n",
    ] {
        serde_yaml::from_str::<JobV2>(invalid)
            .expect_err("an unknown mode or key must not silently declare nothing");
    }
}

fn assert_step_body_shape_error(yaml: &str) {
    let err = serde_yaml::from_str::<JobV2Step>(yaml).expect_err("step should fail to parse");
    assert!(
        err.to_string().contains("exactly one body shape"),
        "unexpected parse error: {err}",
    );
}

#[test]
fn rejects_step_with_parallel_and_target() {
    assert_step_body_shape_error(
        r#"
id: invalid
parallel:
  join: { mode: all }
  branches:
    - id: branch
      target: activity:something
target: activity:other
"#,
    );
}

#[test]
fn rejects_step_with_fan_out_and_loop() {
    assert_step_body_shape_error(
        r#"
id: invalid
fan_out:
  items: "{{ input.items }}"
  worker:
    id: worker
    target: activity:something
fan_in:
  join: { mode: all }
loop:
  max_iterations: 1
  steps:
    - id: loop_child
      target: activity:something
"#,
    );
}

#[test]
fn rejects_step_without_body_shape() {
    assert_step_body_shape_error(
        r#"
id: invalid
when: "{{ input.ready }}"
"#,
    );
}

#[test]
fn target_step_yaml_rejects_step_level_role() {
    let yaml = r#"
id: my_step
role: implementer
spec:
  type: agent_loop
  instruction: hi
"#;
    let error = serde_yaml::from_str::<JobV2Step>(yaml).expect_err("role must be rejected");
    assert!(
        error
            .to_string()
            .contains("pass `crew` in the activity input")
    );
}

#[test]
fn target_ref_yaml_rejects_step_level_role() {
    let yaml = r#"
id: my_step
role: planner
target: activity:something
"#;
    let error = serde_yaml::from_str::<JobV2Step>(yaml).expect_err("role must be rejected");
    assert!(
        error
            .to_string()
            .contains("pass `crew` in the activity input")
    );
}

#[test]
fn step_optional_strings_preserve_omitted_null_and_string_values_in_json_and_yaml() {
    let omitted: JobV2Step =
        serde_json::from_value(json_target_step()).expect("parse step with omitted fields");
    assert_eq!(omitted.when, None);
    assert_eq!(omitted.recovery_activity, None);

    let null_fields: JobV2Step = serde_json::from_value(json!({
        "id": "guarded",
        "when": null,
        "recovery_activity": null,
        "target": "activity:noop"
    }))
    .expect("null optional strings remain absent");
    assert_eq!(null_fields.when, None);
    assert_eq!(null_fields.recovery_activity, None);

    let strings: JobV2Step = serde_json::from_value(json!({
        "id": "guarded",
        "when": "{{ input.ready }}",
        "recovery_activity": "recover",
        "target": "activity:noop"
    }))
    .expect("parse string optional fields");
    assert_eq!(strings.when.as_deref(), Some("{{ input.ready }}"));
    assert_eq!(strings.recovery_activity.as_deref(), Some("recover"));

    let omitted: JobV2Step = serde_yaml::from_str("id: guarded\ntarget: activity:noop\n")
        .expect("parse YAML step with omitted fields");
    assert_eq!(omitted.when, None);
    assert_eq!(omitted.recovery_activity, None);

    let null_fields: JobV2Step = serde_yaml::from_str(
        "id: guarded\nwhen: null\nrecovery_activity: null\ntarget: activity:noop\n",
    )
    .expect("YAML null optional strings remain absent");
    assert_eq!(null_fields.when, None);
    assert_eq!(null_fields.recovery_activity, None);

    let strings: JobV2Step = serde_yaml::from_str(
        "id: guarded\nwhen: '{{ input.ready }}'\nrecovery_activity: recover\ntarget: activity:noop\n",
    )
    .expect("parse YAML string optional fields");
    assert_eq!(strings.when.as_deref(), Some("{{ input.ready }}"));
    assert_eq!(strings.recovery_activity.as_deref(), Some("recover"));
}

#[test]
fn non_string_when_values_are_rejected_with_field_errors_in_json_and_yaml() {
    for invalid_value in [
        json!(false),
        json!(7),
        json!(["ready"]),
        json!({ "ready": true }),
    ] {
        let error =
            serde_json::from_value::<JobV2Step>(json_target_step_with("when", invalid_value))
                .expect_err("non-string JSON guard must fail before dispatch");
        assert_actionable_field_error(error, "when");
    }

    for invalid_yaml in ["false", "7", "[ready]", "{ ready: true }"] {
        let yaml = format!("id: guarded\nwhen: {invalid_yaml}\ntarget: activity:noop\n");
        let error = serde_yaml::from_str::<JobV2Step>(&yaml)
            .expect_err("non-string YAML guard must fail before dispatch");
        assert_actionable_field_error(error, "when");
    }
}

#[test]
fn non_string_recovery_activity_values_are_rejected_in_json_and_yaml() {
    for invalid_value in [
        json!(false),
        json!(7),
        json!(["recover"]),
        json!({ "name": "recover" }),
    ] {
        let error = serde_json::from_value::<JobV2Step>(json_target_step_with(
            "recovery_activity",
            invalid_value,
        ))
        .expect_err("non-string JSON recovery activity must fail");
        assert_actionable_field_error(error, "recovery_activity");
    }

    for invalid_yaml in ["false", "7", "[recover]", "{ name: recover }"] {
        let yaml =
            format!("id: guarded\nrecovery_activity: {invalid_yaml}\ntarget: activity:noop\n");
        let error = serde_yaml::from_str::<JobV2Step>(&yaml)
            .expect_err("non-string YAML recovery activity must fail");
        assert_actionable_field_error(error, "recovery_activity");
    }
}
