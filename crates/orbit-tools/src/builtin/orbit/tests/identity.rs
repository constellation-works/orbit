//! Tests for runtime identity resolution in orbit tools.

use orbit_common::test_fixtures::TEST_CLAUDE_MODEL;
use serde_json::json;

use super::super::*;

#[test]
fn runtime_identity_overwrites_self_reported_model_at_tool_boundary() {
    let ctx = tool_context("claude", TEST_CLAUDE_MODEL);

    let identity =
        resolve_identity(&ctx, &json!({ "model": "opus-4.7" })).expect("identity resolves");

    assert_eq!(identity.agent.as_deref(), Some("claude"));
    assert_eq!(identity.model.as_deref(), Some("claude"));
    assert_eq!(identity.actor_label.as_deref(), Some("claude"));
}

#[test]
fn input_model_preserves_full_strings_and_refuses_unrecognized_families() {
    let ctx = ToolContext {
        cwd: None,
        session_context: Default::default(),
        allowed_tools: Vec::new(),
        workspace_root: None,
        agent_name: None,
        model_name: None,
        trusted_actor_label: None,
        proc_allowed_programs: Vec::new(),
        proc_spawn_environment: None,
        proc_spawn_activity_scoped: false,
        policy_engine: None,
        fs_profile: None,
        fs_audit: None,
        reservation_owner: None,
        orbit_host: None,
    };

    let identity = resolve_identity(&ctx, &json!({ "model": "gpt-5.5" })).expect("normalize");
    assert_eq!(identity.agent.as_deref(), Some("codex"));
    assert_eq!(identity.model.as_deref(), Some("gpt-5.5"));
    assert_eq!(identity.actor_label.as_deref(), Some("codex"));

    let error = resolve_identity(&ctx, &json!({ "model": "llama" })).expect_err("llama refused");
    let message = error.to_string();
    assert!(message.contains("llama"), "{message}");
}

fn tool_context(agent: &str, model: &str) -> ToolContext {
    ToolContext {
        cwd: None,
        session_context: Default::default(),
        allowed_tools: Vec::new(),
        workspace_root: None,
        agent_name: Some(agent.to_string()),
        model_name: Some(model.to_string()),
        trusted_actor_label: None,
        proc_allowed_programs: Vec::new(),
        proc_spawn_environment: None,
        proc_spawn_activity_scoped: false,
        policy_engine: None,
        fs_profile: None,
        fs_audit: None,
        reservation_owner: None,
        orbit_host: None,
    }
}
