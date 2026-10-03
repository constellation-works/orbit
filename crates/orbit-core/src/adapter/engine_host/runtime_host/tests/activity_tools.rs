//! [ORB-13315] Backward compatibility of activity tool policy: every activity
//! written before `tool_disallow_list` existed keeps its exact effective
//! policy and enforcement outcomes.
//!
//! The shipped fixtures are verbatim copies of the pre-change shipped
//! `agent_loop` assets, so later edits to the shipped YAMLs do not move this
//! baseline. The custom fixtures stand for workspace overrides and custom jobs.

use orbit_common::OrbitError;
use orbit_engine::RuntimeHost;
use orbit_engine::activity_job::cli_runner::activity_tool_policy_env;
use orbit_engine::activity_job::load_activity_asset;
use orbit_types::workflow::{
    ActivityToolPolicyMode, ActivityV2Spec, activity_tool_policy_deprecation,
    validate_activity_tool_allowlist_against_registered_tools,
};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::adapter::command::dispatch_test_support::{
    ActivityToolPolicyEnv, activity_tool_policy_from_env_values,
    override_activity_tool_policy_for_test,
};

macro_rules! fixture {
    ($name:literal) => {
        (
            $name,
            include_str!(concat!("fixtures/pre_tool_disallow_list/", $name, ".yaml")),
        )
    };
}

const SHIPPED: &[(&str, &str)] = &[
    fixture!("agent_implement"),
    fixture!("agent_invoke"),
    fixture!("agent_review_repair"),
    fixture!("pr_conflict_recovery"),
    fixture!("step_failure_recovery"),
    fixture!("task_pilot"),
    fixture!("example_agent_apply_fixes"),
    fixture!("example_agent_assess_diff"),
    fixture!("example_agent_loop_cli_reference"),
    fixture!("example_agent_loop_reference"),
];

const CUSTOM: &[(&str, &str)] = &[fixture!("custom_allowlist"), fixture!("custom_empty_tools")];

/// Tools whose enforcement outcome is compared; each runs harmlessly
/// in-memory once the policy admits it (the id-less friction update is
/// refused as invalid input before it writes anything).
fn probes() -> [(&'static str, Value); 4] {
    [
        ("orbit.search", json!({ "query": "compat" })),
        ("orbit.task.show", json!({ "id": "ORB-00001" })),
        ("orbit.task.list", json!({})),
        ("orbit.friction.update", json!({})),
    ]
}

/// The pre-change activity policy, read from the YAML itself rather than
/// through `AgentLoopSpec`, so it is the baseline and not the code under test.
fn pre_change_policy(yaml: &str) -> (Vec<String>, Option<Vec<String>>) {
    let document: serde_yaml::Value = serde_yaml::from_str(yaml).expect("parse fixture");
    let list = |key: &str| {
        document["spec"].get(key).map(|value| {
            value
                .as_sequence()
                .expect("sequence")
                .iter()
                .map(|entry| entry.as_str().expect("string entry").to_string())
                .collect::<Vec<_>>()
        })
    };
    (
        list("tools").unwrap_or_default(),
        list("proc_allowed_programs"),
    )
}

/// The pre-change enforcement rule: an empty allowlist is unrestricted;
/// otherwise a tool passes on an exact entry or a `root.*` prefix.
fn pre_change_admits(allowlist: &[String], tool: &str) -> bool {
    allowlist.is_empty()
        || allowlist.iter().any(|entry| {
            entry == tool
                || entry
                    .strip_suffix('*')
                    .is_some_and(|prefix| tool.starts_with(prefix))
        })
}

#[test]
fn pre_change_activities_keep_their_effective_policy_and_enforcement() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let registered = runtime.allowlist_known_tool_names();

    for (fixture, yaml) in SHIPPED.iter().chain(CUSTOM) {
        let (tools, programs) = pre_change_policy(yaml);
        let asset = load_activity_asset(yaml)
            .unwrap_or_else(|error| panic!("{fixture} must load unchanged: {error}"));
        validate_activity_tool_allowlist_against_registered_tools(
            &asset.spec,
            registered.iter().map(String::as_str),
        )
        .unwrap_or_else(|error| panic!("{fixture} must pass registry validation: {error}"));
        let ActivityV2Spec::AgentLoop(spec) = &asset.spec.spec else {
            panic!("{fixture} is an agent_loop activity");
        };

        // Effective policy: allowlist mode with the declared lists verbatim.
        assert_eq!(
            spec.tool_policy_mode(),
            ActivityToolPolicyMode::Allow,
            "{fixture}"
        );
        assert_eq!(spec.tool_disallow_list, None, "{fixture}");
        assert_eq!(spec.tools, tools, "{fixture}");
        assert_eq!(spec.proc_allowed_programs, programs, "{fixture}");
        assert_eq!(
            activity_tool_policy_deprecation(&asset.spec).is_some(),
            tools.is_empty(),
            "{fixture}: only an empty `tools:` list is warned about"
        );

        let resolved = RuntimeHost::resolve_activity_tools(&runtime, &[], &spec.tools)
            .unwrap_or_else(|error| panic!("{fixture} resolves: {error}"));
        assert_eq!(resolved.effective_tools, tools, "{fixture}");

        // The managed envelope carries exactly the legacy allowlist, and the
        // MCP server reads it back as the same allowlist with no deny policy.
        let stamped = activity_tool_policy_env(
            &asset.name,
            spec.tool_disallow_list.as_deref(),
            &resolved.effective_tools,
        );
        assert_eq!(
            stamped,
            [("ORBIT_ACTIVITY_TOOLS".to_string(), tools.join(","))],
            "{fixture}"
        );
        let parsed = activity_tool_policy_from_env_values(None, None, None, Some(&stamped[0].1));
        assert_eq!(
            parsed,
            ActivityToolPolicyEnv {
                allowed_tools: tools.clone(),
                deny_policy: None,
            },
            "{fixture}"
        );

        // Enforcement: each probe is refused by activity policy exactly when
        // the pre-change rule refused it, with the pre-change message.
        let _policy = override_activity_tool_policy_for_test(parsed);
        for (tool, input) in probes() {
            let outcome = runtime.execute_tool_command(
                tool,
                input,
                Some("codex".to_string()),
                Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
            );
            let refused_by_policy = matches!(
                &outcome,
                Err(OrbitError::PolicyDenied(message))
                    if *message == format!("tool '{tool}' is not in the activity allowlist")
            );
            assert_eq!(
                refused_by_policy,
                !pre_change_admits(&tools, tool),
                "{fixture}: `{tool}` enforcement changed: {outcome:?}"
            );
        }
    }
}
