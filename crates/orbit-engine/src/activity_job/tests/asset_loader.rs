use super::super::asset_loader::*;

fn agent_loop_activity_yaml(name: &str, tools: &str) -> String {
    format!(
        r#"schemaVersion: 2
kind: Activity
metadata:
  name: {name}
spec:
  type: agent_loop
  description: Test agent loop.
  instruction: Test.
  tools:
{tools}"#
    )
}

#[test]
fn load_activity_asset_accepts_task_wildcard_tool_allowlist() {
    let yaml = agent_loop_activity_yaml("task_tools", "    - orbit.task.*\n");

    let asset = load_activity_asset(&yaml).expect("activity should load");

    assert_eq!(asset.name, "task_tools");
}

#[test]
fn load_activity_asset_rejects_top_level_orbit_wildcard() {
    let yaml = agent_loop_activity_yaml("broad_tools", "    - orbit.*\n");

    let err = load_activity_asset(&yaml).expect_err("broad wildcard should fail");
    let message = err.to_string();

    assert!(message.contains("orbit.*"), "{message}");
    assert!(message.contains("wildcard root not permitted"), "{message}");
}

#[test]
fn load_activity_asset_rejects_empty_tool_name() {
    let yaml = agent_loop_activity_yaml("empty_tools", "    - \"\"\n");

    let err = load_activity_asset(&yaml).expect_err("empty tool should fail");
    let message = err.to_string();

    assert!(message.contains("empty tool name"), "{message}");
    assert!(message.contains("index 0"), "{message}");
}

#[test]
fn activity_role_error_names_asset_and_crew_replacement() {
    let yaml = agent_loop_activity_yaml("legacy_activity", "    - orbit.task.show\n")
        + "  role: implementer\n";
    let error = load_activity_asset(&yaml).expect_err("retired role must fail");
    let message = error.to_string();
    assert!(message.contains("activity `legacy_activity`"), "{message}");
    assert!(
        message.contains("pass `crew` in the activity input"),
        "{message}"
    );
    assert!(message.contains("run's resolved crew"), "{message}");
}

#[test]
fn nested_job_role_error_names_asset_and_crew_replacement() {
    let yaml = r#"schemaVersion: 2
kind: Job
metadata:
  name: legacy_job
spec:
  state: enabled
  steps:
    - id: outer
      loop:
        max_iterations: 1
        steps:
          - id: legacy
            role: planner
            target: activity:anything
"#;
    let error = load_job_asset(yaml).expect_err("retired role must fail");
    let message = error.to_string();
    assert!(message.contains("job `legacy_job`"), "{message}");
    assert!(
        message.contains("pass `crew` in the activity input"),
        "{message}"
    );
}

/// [ORB-10959] An asset that grants `proc.spawn` while omitting
/// `proc_allowed_programs` is refused at load. Before this, the omitted key
/// meant "unconstrained" while an explicit `[]` denied everything — the
/// safer-looking asset was the more permissive one.
#[test]
fn load_activity_asset_rejects_proc_spawn_without_program_allowlist() {
    let yaml = agent_loop_activity_yaml("unbounded_spawn", "    - proc.spawn\n");

    let err = load_activity_asset(&yaml).expect_err("missing program allowlist should fail");
    let message = err.to_string();

    assert!(message.contains("proc.spawn"), "{message}");
    assert!(message.contains("proc_allowed_programs"), "{message}");
}

/// Writing `proc_allowed_programs: []` is the supported way to grant the tool
/// and deny every program, so it must still load.
#[test]
fn load_activity_asset_accepts_explicit_empty_program_allowlist() {
    let yaml = format!(
        "{}\n  proc_allowed_programs: []\n",
        agent_loop_activity_yaml("deny_all_spawn", "    - proc.spawn\n")
    );

    let asset = load_activity_asset(&yaml).expect("explicit deny-all should load");

    assert_eq!(asset.name, "deny_all_spawn");
}

/// [ORB-11354] The unsandboxed execution mode is legal on exactly one built-in
/// activity name. Asset load is where that is enforced, because activity assets
/// live in a workspace directory an operator can edit — a key in YAML must
/// never be able to name a second activity into the mode.
fn trusted_host_activity_yaml(name: &str) -> String {
    format!(
        r#"schemaVersion: 2
kind: Activity
metadata:
  name: {name}
spec:
  type: agent_loop
  trustedHostExecution: true
  description: Test agent loop.
  instruction: Test.
  tools:
    - orbit.task.show
"#
    )
}

#[test]
fn load_activity_asset_accepts_trusted_host_execution_on_the_builtin_activity() {
    let asset = load_activity_asset(&trusted_host_activity_yaml("agent_invoke"))
        .expect("the built-in exploration activity declares the mode");

    let orbit_types::workflow::activity_job::ActivityV2Spec::AgentLoop(spec) = asset.spec.spec
    else {
        panic!("expected an agent_loop activity");
    };
    assert!(spec.trusted_host_execution);
}

#[test]
fn load_activity_asset_rejects_trusted_host_execution_on_any_other_activity() {
    let error = load_activity_asset(&trusted_host_activity_yaml("agent_implement"))
        .expect_err("no other activity may claim the mode");

    assert!(
        matches!(error, AssetLoadError::TrustedHostActivity(_)),
        "expected a trusted-host refusal, got {error:?}"
    );
    assert!(
        error.to_string().contains("activity `agent_implement`"),
        "the refusal must name the refused asset: {error}"
    );
}

#[test]
fn an_activity_that_omits_the_key_does_not_declare_the_mode() {
    let asset = load_activity_asset(&agent_loop_activity_yaml(
        "agent_implement",
        "    - orbit.task.show\n",
    ))
    .expect("activity should load");

    let orbit_types::workflow::activity_job::ActivityV2Spec::AgentLoop(spec) = asset.spec.spec
    else {
        panic!("expected an agent_loop activity");
    };
    assert!(!spec.trusted_host_execution);
}
