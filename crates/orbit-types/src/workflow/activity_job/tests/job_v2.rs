use super::super::job_v2::*;

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
