//! Sibling tests for `activity_catalog.rs`: the activity catalog a runtime
//! resolves `target: activity:<name>` against.

use std::path::Path;

use super::runtime::test_runtime;
use crate::runtime::assets::DEFAULT_ACTIVITY_FILES;
use orbit_engine::activity_job::load_activity_asset;
use orbit_types::workflow::{
    ActivityToolDenyPolicy, ActivityToolPolicyMode, ActivityV2Spec, tool_allowed,
    tools_allowed_by_disallow_list,
};

const AGENT_ACTIVITIES: [&str; 6] = [
    "agent_implement",
    "agent_invoke",
    "agent_review_repair",
    "pr_conflict_recovery",
    "step_failure_recovery",
    "task_pilot",
];

const CONTROL_TOOLS: [&str; 11] = [
    "orbit.agent.invoke",
    "orbit.auto_task.add",
    "orbit.auto_task.delete",
    "orbit.auto_task.mint",
    "orbit.auto_task.toggle",
    "orbit.auto_task.update",
    "orbit.command.exec",
    "orbit.pipeline.invoke",
    "orbit.workflow.run.resume",
    "orbit.workflow.run.workers",
    "orbit.workflow.ship",
];

const READ_ONLY_MUTATIONS: [&str; 6] = [
    "orbit.task.add",
    "orbit.task.update",
    "orbit.task.artifact.put",
    "orbit.friction.add",
    "orbit.friction.update",
    "orbit.friction.rehome",
];

fn write_activity(path: &Path, name: &str, description: &str) {
    let yaml = format!(
        r#"schemaVersion: 2
kind: Activity
metadata:
  name: {name}
spec:
  type: deterministic
  description: {description}
  action: test_action
  config: {{}}
"#
    );
    std::fs::create_dir_all(path.parent().expect("activity path has parent"))
        .expect("create activity dir");
    std::fs::write(path, yaml).expect("write activity yaml");
}

fn write_agent_loop_activity(path: &Path, name: &str, tools: &[&str]) {
    let tools_yaml = tools
        .iter()
        .map(|tool| format!("    - {tool}\n"))
        .collect::<String>();
    let yaml = format!(
        r#"schemaVersion: 2
kind: Activity
metadata:
  name: {name}
spec:
  type: agent_loop
  description: Test agent loop.
  instruction: Test.
  tools:
{tools_yaml}"#
    );
    std::fs::create_dir_all(path.parent().expect("activity path has parent"))
        .expect("create activity dir");
    std::fs::write(path, yaml).expect("write activity yaml");
}

#[test]
fn global_default_activity_wins_over_workspace_shadow_in_execution_catalog() {
    let (_root, runtime, global_root, workspace_root) = test_runtime();
    write_activity(
        &global_root.join("resources/activities/pr_open.yaml"),
        "pr_open",
        "global description",
    );
    write_activity(
        &workspace_root.join("resources/activities/pr_open.yaml"),
        "pr_open",
        "workspace description",
    );

    let catalog = runtime.v2_activity_catalog().expect("activity catalog");
    let activity = catalog.get("pr_open").expect("pr_open activity");
    assert_eq!(activity.description, "global description");
}

#[test]
fn workspace_default_activity_cannot_claim_missing_global_default_name() {
    let (_root, runtime, _global_root, workspace_root) = test_runtime();
    write_activity(
        &workspace_root.join("resources/activities/pr_open.yaml"),
        "pr_open",
        "workspace description",
    );

    let catalog = runtime.v2_activity_catalog().expect("activity catalog");

    assert!(
        catalog.get("pr_open").is_none(),
        "workspace assets must never claim shipped default activity names"
    );
}

#[test]
fn activity_catalog_still_skips_retired_assets() {
    let (_root, runtime, _global_root, workspace_root) = test_runtime();
    let activities_dir = workspace_root.join("resources/activities");
    std::fs::create_dir_all(&activities_dir).expect("create activities dir");
    std::fs::write(
        activities_dir.join("retired.yaml"),
        "schemaVersion: 1\nkind: Activity\nmetadata:\n  name: retired\nspec: {}\n",
    )
    .expect("write retired activity");
    write_activity(
        &activities_dir.join("current.yaml"),
        "current",
        "current description",
    );

    let catalog = runtime.v2_activity_catalog().expect("activity catalog");

    assert!(catalog.get("retired").is_none());
    assert!(catalog.get("current").is_some());
}

#[test]
fn duplicate_activities_within_one_catalog_directory_remain_invalid() {
    let (_root, runtime, _global_root, workspace_root) = test_runtime();
    let activities_dir = workspace_root.join("resources/activities");
    write_activity(
        &activities_dir.join("first.yaml"),
        "duplicate_activity",
        "first description",
    );
    write_activity(
        &activities_dir.join("nested/second.yaml"),
        "duplicate_activity",
        "second description",
    );

    let err = runtime
        .v2_activity_catalog()
        .expect_err("duplicate activity name should fail");
    assert!(err.to_string().contains("duplicate activity name"), "{err}");
}

#[test]
fn activity_catalog_accepts_registered_task_wildcard() {
    let (_root, runtime, _global_root, workspace_root) = test_runtime();
    write_agent_loop_activity(
        &workspace_root.join("resources/activities/task_tools.yaml"),
        "task_tools",
        &["orbit.task.*"],
    );

    let catalog = runtime.v2_activity_catalog().expect("activity catalog");

    assert!(catalog.get("task_tools").is_some());
}

#[test]
fn activity_catalog_rejects_unknown_concrete_tool() {
    let (_root, runtime, _global_root, workspace_root) = test_runtime();
    write_agent_loop_activity(
        &workspace_root.join("resources/activities/unknown_tool.yaml"),
        "unknown_tool",
        &["orbit.task.nope"],
    );

    let err = runtime
        .v2_activity_catalog()
        .expect_err("unknown concrete tool should fail");
    let message = err.to_string();

    assert!(message.contains("unknown_tool"), "{message}");
    assert!(message.contains("orbit.task.nope"), "{message}");
    assert!(message.contains("unknown tool name"), "{message}");
}

#[test]
fn activity_catalog_accepts_intentionally_empty_audit_wildcard() {
    let (_root, runtime, _global_root, workspace_root) = test_runtime();
    write_agent_loop_activity(
        &workspace_root.join("resources/activities/audit_tools.yaml"),
        "audit_tools",
        &["orbit.audit.*"],
    );

    let catalog = runtime.v2_activity_catalog().expect("activity catalog");

    assert!(catalog.get("audit_tools").is_some());
}

#[test]
fn default_activity_catalog_allowlists_resolve_registered_tools() {
    let (_root, runtime, global_root, _workspace_root) = test_runtime();
    let activities_dir = global_root.join("resources/activities");
    for (name, yaml) in DEFAULT_ACTIVITY_FILES {
        let path = activities_dir.join(format!("{name}.yaml"));
        std::fs::create_dir_all(path.parent().expect("activity path has parent"))
            .expect("create activity dir");
        std::fs::write(path, yaml).expect("write activity yaml");
    }

    let catalog = runtime.v2_activity_catalog().expect("activity catalog");

    assert_eq!(catalog.len(), DEFAULT_ACTIVITY_FILES.len());
}

#[test]
fn shipped_agent_activities_deny_control_tools_and_keep_prompt_tools_callable() {
    let (_root, runtime, _global_root, _workspace_root) = test_runtime();
    let registered = runtime.allowlist_known_tool_names();

    for name in AGENT_ACTIVITIES {
        let yaml = DEFAULT_ACTIVITY_FILES
            .iter()
            .find_map(|(candidate, yaml)| (*candidate == name).then_some(*yaml))
            .expect("shipped agent activity");
        let activity = load_activity_asset(yaml).expect("shipped activity loads");
        let ActivityV2Spec::AgentLoop(spec) = &activity.spec.spec else {
            panic!("{name} must be an agent loop");
        };
        assert_eq!(
            spec.tool_policy_mode(),
            ActivityToolPolicyMode::Deny,
            "{name}"
        );
        assert!(spec.tools.is_empty(), "{name} must not retain an allowlist");
        let disallowed = spec.tool_disallow_list.as_ref().expect("deny list");
        let policy = ActivityToolDenyPolicy {
            activity: name.to_string(),
            disallow_list: disallowed.clone(),
        };
        for tool in CONTROL_TOOLS {
            assert!(policy.denies(tool), "{name} must deny {tool}");
        }
        for tool in READ_ONLY_MUTATIONS {
            assert_eq!(
                policy.denies(tool),
                matches!(name, "agent_invoke" | "task_pilot"),
                "{name}: {tool}"
            );
        }
        assert!(!policy.denies("proc.spawn"), "{name} still uses proc.spawn");
        assert!(
            spec.proc_allowed_programs.is_some(),
            "{name} must bound proc.spawn"
        );

        // Parse the instruction as tool-name tokens, then check each named
        // registered tool against the same policy used for dispatch. This
        // guards the executable instruction contract without pinning prose.
        let named_tools = spec
            .instruction
            .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '.' || ch == '_'))
            .filter(|token| registered.iter().any(|tool| tool == token));
        for tool in named_tools {
            assert!(
                !policy.denies(tool),
                "{name} instructs the agent to call denied {tool}"
            );
        }

        let effective =
            tools_allowed_by_disallow_list(disallowed, registered.iter().map(String::as_str));
        assert!(effective.iter().any(|tool| tool == "proc.spawn"), "{name}");
        assert!(
            !effective.iter().any(|tool| tool == "orbit.agent.invoke"),
            "{name}"
        );
    }
}

// Verbatim YAML from origin/agent-main at 4ad2e84d7, before the shipped
// migration. Keep these fixtures intact so the legacy reader and enforcement
// contract are checked against actual previously seeded activities.
const LEGACY_AGENT_ACTIVITIES: [(&str, &str, &[&str]); 6] = [
    (
        "agent_implement",
        include_str!("fixtures/legacy_allowlist/agent_implement.yaml"),
        &[
            "orbit.task.*",
            "orbit.friction.*",
            "orbit.search",
            "proc.spawn",
        ],
    ),
    (
        "agent_invoke",
        include_str!("fixtures/legacy_allowlist/agent_invoke.yaml"),
        &[
            "orbit.task.show",
            "orbit.task.list",
            "orbit.search",
            "proc.spawn",
        ],
    ),
    (
        "agent_review_repair",
        include_str!("fixtures/legacy_allowlist/agent_review_repair.yaml"),
        &["orbit.task.*", "orbit.search", "proc.spawn"],
    ),
    (
        "pr_conflict_recovery",
        include_str!("fixtures/legacy_allowlist/pr_conflict_recovery.yaml"),
        &["orbit.task.*", "proc.spawn"],
    ),
    (
        "step_failure_recovery",
        include_str!("fixtures/legacy_allowlist/step_failure_recovery.yaml"),
        &["orbit.task.*", "orbit.friction.*", "proc.spawn"],
    ),
    (
        "task_pilot",
        include_str!("fixtures/legacy_allowlist/task_pilot.yaml"),
        &[
            "orbit.task.show",
            "orbit.task.list",
            "orbit.search",
            "proc.spawn",
        ],
    ),
];

#[test]
fn pre_migration_shipped_and_custom_allowlists_keep_their_effective_policy() {
    let (_root, runtime, _global_root, _workspace_root) = test_runtime();
    let registered = runtime.allowlist_known_tool_names();
    let custom = r#"schemaVersion: 2
kind: Activity
metadata:
  name: custom_allowlist
spec:
  type: agent_loop
  description: Custom activity.
  instruction: Inspect a task.
  tools:
    - orbit.task.show
    - proc.spawn
  proc_allowed_programs:
    - git
"#;
    let cases = LEGACY_AGENT_ACTIVITIES.iter().copied().chain([(
        "custom_allowlist",
        custom,
        &["orbit.task.show", "proc.spawn"][..],
    )]);

    for (name, yaml, expected) in cases {
        let activity = load_activity_asset(yaml).expect("legacy allowlist loads unchanged");
        let ActivityV2Spec::AgentLoop(spec) = &activity.spec.spec else {
            panic!("{name} must be an agent loop");
        };
        assert_eq!(
            spec.tool_policy_mode(),
            ActivityToolPolicyMode::Allow,
            "{name}"
        );
        assert!(spec.tool_disallow_list.is_none(), "{name}");
        assert_eq!(
            spec.tools,
            expected
                .iter()
                .map(|tool| (*tool).to_string())
                .collect::<Vec<_>>(),
            "{name} must preserve its pre-migration allowlist"
        );
        assert!(spec.proc_allowed_programs.is_some(), "{name}");
        let effective = registered
            .iter()
            .filter(|tool| tool_allowed(tool, &spec.tools))
            .map(String::as_str)
            .collect::<Vec<_>>();
        for tool in &registered {
            assert_eq!(
                effective.contains(&tool.as_str()),
                tool_allowed(
                    tool,
                    &expected
                        .iter()
                        .map(|entry| (*entry).to_string())
                        .collect::<Vec<_>>()
                ),
                "{name} changed the enforcement outcome for {tool}"
            );
        }
        assert!(!tool_allowed("orbit.workflow.ship", &spec.tools), "{name}");
        assert!(!tool_allowed("github.auth.status", &spec.tools), "{name}");
        assert!(tool_allowed("proc.spawn", &spec.tools), "{name}");
    }
}

#[test]
fn workspace_custom_allowlist_remains_an_allowlist() {
    let (_root, runtime, _global_root, workspace_root) = test_runtime();
    write_agent_loop_activity(
        &workspace_root.join("resources/activities/custom_allowlist.yaml"),
        "custom_allowlist",
        &["orbit.task.show"],
    );
    let catalog = runtime.v2_activity_catalog().expect("workspace catalog");
    let activity = catalog.get("custom_allowlist").expect("custom activity");
    let ActivityV2Spec::AgentLoop(spec) = &activity.spec else {
        panic!("custom activity must be an agent loop");
    };
    assert_eq!(spec.tool_policy_mode(), ActivityToolPolicyMode::Allow);
    assert!(tool_allowed("orbit.task.show", &spec.tools));
    assert!(!tool_allowed("orbit.task.update", &spec.tools));
}

#[test]
fn workspace_shadow_of_a_shipped_activity_keeps_existing_catalog_precedence() {
    let (_root, runtime, global_root, workspace_root) = test_runtime();
    let current = DEFAULT_ACTIVITY_FILES
        .iter()
        .find_map(|(name, yaml)| (*name == "agent_implement").then_some(*yaml))
        .expect("shipped activity");
    let global_path = global_root.join("resources/activities/agent_implement.yaml");
    let workspace_path = workspace_root.join("resources/activities/agent_implement.yaml");
    std::fs::create_dir_all(global_path.parent().expect("global parent"))
        .expect("create global activity dir");
    std::fs::create_dir_all(workspace_path.parent().expect("workspace parent"))
        .expect("create workspace activity dir");
    std::fs::write(global_path, current).expect("seed current activity");
    std::fs::write(
        workspace_path,
        include_str!("fixtures/legacy_allowlist/agent_implement.yaml"),
    )
    .expect("install legacy workspace shadow");

    let catalog = runtime.v2_activity_catalog().expect("catalog");
    let activity = catalog.get("agent_implement").expect("shipped activity");
    let ActivityV2Spec::AgentLoop(spec) = &activity.spec else {
        panic!("agent implement must be an agent loop");
    };
    assert_eq!(spec.tool_policy_mode(), ActivityToolPolicyMode::Deny);
}
