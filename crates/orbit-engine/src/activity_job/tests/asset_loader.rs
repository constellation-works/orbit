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

#[test]
fn load_activity_asset_accepts_program_disallow_mode_and_rejects_both_modes() {
    let yaml = format!(
        "{}\n  proc_disallowed_programs: []\n",
        agent_loop_activity_yaml("deny_mode_spawn", "    - proc.spawn\n")
    );
    let asset = load_activity_asset(&yaml).expect("explicit empty disallow list loads");
    let orbit_types::workflow::ActivityV2Spec::AgentLoop(spec) = asset.spec.spec else {
        panic!("expected agent loop")
    };
    assert_eq!(spec.proc_disallowed_programs, Some(Vec::new()));

    let both = format!("{yaml}  proc_allowed_programs: []\n");
    let error = load_activity_asset(&both).expect_err("two program modes are ambiguous");
    assert!(error.to_string().contains("mutually exclusive"), "{error}");
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

fn deny_mode_activity_yaml(name: &str, extra: &str) -> String {
    format!(
        r#"schemaVersion: 2
kind: Activity
metadata:
  name: {name}
spec:
  type: agent_loop
  description: Test agent loop.
  instruction: Test.
{extra}"#
    )
}

/// [ORB-13315] A disallow-list activity loads in deny mode.
#[test]
fn load_activity_asset_accepts_a_tool_disallow_list() {
    let yaml = deny_mode_activity_yaml(
        "deny_tools",
        "  tool_disallow_list:\n    - orbit.workflow.ship\n    - proc.*\n",
    );

    let asset = load_activity_asset(&yaml).expect("deny-mode activity should load");

    let orbit_types::workflow::ActivityV2Spec::AgentLoop(spec) = &asset.spec.spec else {
        panic!("agent loop");
    };
    assert_eq!(
        spec.tool_policy_mode(),
        orbit_types::workflow::ActivityToolPolicyMode::Deny
    );
}

/// [ORB-13315] Declaring both lists is a load error naming the activity.
#[test]
fn load_activity_asset_rejects_tools_with_a_disallow_list() {
    let yaml = deny_mode_activity_yaml(
        "both_lists",
        "  tools:\n    - orbit.task.show\n  tool_disallow_list:\n    - orbit.search\n",
    );

    let err = load_activity_asset(&yaml).expect_err("both lists must fail");

    assert!(matches!(
        &err,
        AssetLoadError::ToolAllowlist {
            activity,
            source: orbit_types::workflow::ToolAllowlistError::ToolsAndDisallowListBothSet,
        } if activity == "both_lists"
    ));
}

/// [ORB-13315] Disallow entries obey the allowlist's wildcard roots.
#[test]
fn load_activity_asset_rejects_a_disallow_wildcard_outside_permitted_roots() {
    let yaml = deny_mode_activity_yaml("broad_disallow", "  tool_disallow_list:\n    - orbit.*\n");

    let err = load_activity_asset(&yaml).expect_err("broad wildcard must fail");

    assert!(matches!(
        &err,
        AssetLoadError::ToolAllowlist {
            activity,
            source: orbit_types::workflow::ToolAllowlistError::DisallowList { .. },
        } if activity == "broad_disallow"
    ));
}

/// [ORB-13315] An empty `tools:` list keeps loading: it is deprecated with a
/// warning, not refused or reinterpreted.
#[test]
fn load_activity_asset_keeps_loading_an_empty_tool_allowlist() {
    let yaml = agent_loop_activity_yaml("empty_allowlist", "    []\n");

    let asset = load_activity_asset(&yaml).expect("empty allowlist still loads");

    assert!(orbit_types::workflow::activity_tool_policy_deprecation(&asset.spec).is_some());
}

/// A plain scalar that reads as a number is still text where the schema says
/// text. Typed parsing takes it verbatim; going through an untyped document
/// first would turn it into a number and refuse it.
#[test]
fn load_activity_asset_keeps_number_like_scalars_in_text_fields() {
    let yaml = agent_loop_activity_yaml("versioned", "    - orbit.task.show\n")
        .replace("Test agent loop.", "2024");

    let asset = load_activity_asset(&yaml).expect("number-like text fields load as text");

    assert_eq!(asset.spec.description, "2024");
}

#[test]
fn load_job_asset_keeps_number_like_scalars_in_text_fields() {
    let yaml = r#"schemaVersion: 2
kind: Job
metadata:
  name: 2024
  description: 1.0
spec:
  state: enabled
  steps:
    - id: only
      target: activity:anything
"#;

    let asset = load_job_asset(yaml).expect("number-like names load as text");

    assert_eq!(asset.name, "2024");
}

#[test]
fn asset_syntax_errors_keep_their_source_position() {
    let yaml = "schemaVersion: 2\nkind: Job\nmetadata: [unclosed\n";

    let error = load_job_asset(yaml).expect_err("malformed YAML must fail");

    assert!(matches!(error, AssetLoadError::HeaderParse(_)), "{error:?}");
    assert!(error.to_string().contains("line"), "{error}");
}

#[test]
fn asset_type_errors_keep_their_source_position() {
    let yaml = agent_loop_activity_yaml("typed", "    - orbit.task.show\n")
        .replace("type: agent_loop", "type: [not, a, string]");

    let error = load_activity_asset(&yaml).expect_err("a mistyped field must fail");

    assert!(matches!(error, AssetLoadError::Parse(_)), "{error:?}");
    assert!(error.to_string().contains("line"), "{error}");
}

#[test]
fn retired_and_unknown_schema_versions_are_reported_before_the_body_is_read() {
    let retired = load_activity_asset("schemaVersion: 1\nkind: Activity\nspec: not-a-mapping\n")
        .expect_err("schemaVersion 1 is retired");
    assert!(
        matches!(retired, AssetLoadError::RetiredVersion(1)),
        "{retired:?}"
    );

    let unknown = load_job_asset("schemaVersion: 9\nkind: Job\nspec: not-a-mapping\n")
        .expect_err("schemaVersion 9 is unknown");
    assert!(
        matches!(unknown, AssetLoadError::UnsupportedVersion(9)),
        "{unknown:?}"
    );

    let duplicated = load_activity_asset("schemaVersion: 1\nkind: Activity\nkind: Job\n")
        .expect_err("a retired asset stays retired whatever else is wrong with it");
    assert!(
        matches!(duplicated, AssetLoadError::RetiredVersion(1)),
        "{duplicated:?}"
    );
}

#[test]
fn a_kind_mismatch_is_reported_for_a_well_formed_asset_of_the_wrong_kind() {
    let yaml = agent_loop_activity_yaml("misfiled", "    - orbit.task.show\n")
        .replace("kind: Activity", "kind: Job");

    let error = load_activity_asset(&yaml).expect_err("a job is not an activity");

    assert!(
        matches!(error, AssetLoadError::KindMismatch { .. }),
        "{error:?}"
    );
}
