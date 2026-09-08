use std::collections::BTreeMap;

use super::super::child_env::{
    AGENT_SUBPROCESS_BASELINE_VARS, allowlisted_child_env_from, backfill_login_identity,
    os_login_name,
};

/// A parent environment carrying benignly named credentials alongside ordinary
/// runtime context. Stated as data so the admission rule is asserted against a
/// known environment rather than the developer's shell.
fn parent_env() -> Vec<(String, String)> {
    [
        ("HOME", "/home/agent"),
        ("PATH", "/usr/bin:/bin"),
        ("USER", "agent"),
        ("LOGNAME", "agent"),
        ("LANG", "en_US.UTF-8"),
        // Benignly named — no SECRET/TOKEN/KEY substring anywhere.
        ("DATABASE_URL", "postgres://svc:hunter2@db.internal/prod"),
        ("BILLING_ENDPOINT", "https://billing.internal.example"),
        ("STRIPE_LIVE", "rk_live_00000000000000000000"),
        // Credential-shaped, to prove the gate is membership and not shape.
        ("ANTHROPIC_API_KEY", "sk-ant-000000000000000000000000"),
        ("GH_TOKEN", "ghp_000000000000000000000000000000000000"),
        // Orbit's own execution envelope.
        ("ORBIT_RUN_ID", "jrun-1"),
        ("ORBIT_TASK_ID", "ORB-00001"),
        // Provider-declared extra, opted into by name at the call site.
        ("CODEX_HOME", "/home/agent/.codex"),
    ]
    .into_iter()
    .map(|(name, value)| (name.to_string(), value.to_string()))
    .collect()
}

fn names(env: &[(String, String)]) -> Vec<&str> {
    env.iter().map(|(name, _)| name.as_str()).collect()
}

fn value_of<'a>(env: &'a [(String, String)], key: &str) -> Option<&'a str> {
    env.iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.as_str())
}

#[test]
fn benignly_named_ambient_credentials_are_not_admitted() {
    let env = allowlisted_child_env_from(&parent_env(), &[], &[]);

    // The whole point of ORB-10917: these have no credential-shaped name, so a
    // denylist forwards them. An allowlist does not.
    for absent in ["DATABASE_URL", "BILLING_ENDPOINT", "STRIPE_LIVE"] {
        assert!(
            !names(&env).contains(&absent),
            "{absent} must not reach an agent subprocess; got {:?}",
            names(&env)
        );
    }
}

#[test]
fn credential_shaped_ambient_names_are_not_admitted_either() {
    let env = allowlisted_child_env_from(&parent_env(), &[], &[]);

    for absent in ["ANTHROPIC_API_KEY", "GH_TOKEN"] {
        assert!(
            !names(&env).contains(&absent),
            "{absent} must not reach an agent subprocess by default"
        );
    }
}

#[test]
fn baseline_runtime_context_reaches_the_child() {
    let env = allowlisted_child_env_from(&parent_env(), &[], &[]);

    assert_eq!(value_of(&env, "HOME"), Some("/home/agent"));
    assert_eq!(value_of(&env, "PATH"), Some("/usr/bin:/bin"));
    assert_eq!(value_of(&env, "USER"), Some("agent"));
    assert_eq!(value_of(&env, "LANG"), Some("en_US.UTF-8"));
    // A baseline name the parent does not hold is simply absent, never empty.
    assert!(!names(&env).contains(&"TERM"));
    assert!(AGENT_SUBPROCESS_BASELINE_VARS.contains(&"TERM"));
}

#[test]
fn configured_pass_list_admits_a_named_ambient_variable() {
    let pass = vec!["DATABASE_URL".to_string()];

    let env = allowlisted_child_env_from(&parent_env(), &pass, &[]);

    assert_eq!(
        value_of(&env, "DATABASE_URL"),
        Some("postgres://svc:hunter2@db.internal/prod"),
        "an operator naming a variable in execution.env.pass is the admission path"
    );
    assert!(!names(&env).contains(&"BILLING_ENDPOINT"));
}

#[test]
fn provider_required_extras_reach_the_child() {
    let env = allowlisted_child_env_from(&parent_env(), &[], &["CODEX_HOME"]);

    assert_eq!(value_of(&env, "CODEX_HOME"), Some("/home/agent/.codex"));
}

#[test]
fn orbit_execution_envelope_variables_reach_the_child() {
    let env = allowlisted_child_env_from(&parent_env(), &[], &[]);

    assert_eq!(value_of(&env, "ORBIT_RUN_ID"), Some("jrun-1"));
    assert_eq!(value_of(&env, "ORBIT_TASK_ID"), Some("ORB-00001"));
}

#[test]
fn privilege_bearing_orbit_variables_do_not_ride_the_envelope_allowlist() {
    let parent = [
        ("ORBIT_OPERATOR", "1"),
        ("ORBIT_WORKSPACE_CLAIM_TOKEN", "abc123"),
        ("ORBIT_MCP_SSH_ACCEPTANCE", "x"),
        ("ORBIT_RUN_ID", "r1"),
    ]
    .into_iter()
    .map(|(name, value)| (name.to_string(), value.to_string()))
    .collect::<Vec<_>>();

    let env = allowlisted_child_env_from(&parent, &[], &[]);

    assert_eq!(value_of(&env, "ORBIT_RUN_ID"), Some("r1"));
    let orbit_names: Vec<&str> = names(&env)
        .into_iter()
        .filter(|name| name.starts_with("ORBIT_"))
        .collect();
    assert_eq!(
        orbit_names,
        ["ORBIT_RUN_ID"],
        "privilege-bearing ORBIT_* names must not survive allowlisted_child_env_from"
    );
}

#[test]
fn admitted_names_are_deterministic_and_unique() {
    let pass = vec!["HOME".to_string(), "DATABASE_URL".to_string()];

    let first = allowlisted_child_env_from(&parent_env(), &pass, &["HOME", "CODEX_HOME"]);
    let second = allowlisted_child_env_from(&parent_env(), &pass, &["CODEX_HOME", "HOME"]);

    assert_eq!(first, second, "ordering must not depend on input order");
    let mut sorted = names(&first);
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        first.len(),
        "a name repeated across baseline/pass/extras must appear once"
    );
}

#[test]
fn login_identity_is_backfilled_when_the_parent_has_none() {
    let Some(expected) = os_login_name() else {
        // No resolvable login on this host; backfill is a no-op by design.
        return;
    };
    let parent = vec![("PATH".to_string(), "/usr/bin".to_string())];

    let env = allowlisted_child_env_from(&parent, &[], &[]);

    assert_eq!(value_of(&env, "USER"), Some(expected.as_str()));
    assert_eq!(value_of(&env, "LOGNAME"), Some(expected.as_str()));
}

#[test]
fn backfill_login_identity_preserves_an_existing_nonempty_identity() {
    let mut vars = BTreeMap::from([
        ("USER".to_string(), "explicit-user".to_string()),
        ("LOGNAME".to_string(), "explicit-user".to_string()),
    ]);

    backfill_login_identity(&mut vars);

    assert_eq!(vars.get("USER").map(String::as_str), Some("explicit-user"));
    assert_eq!(
        vars.get("LOGNAME").map(String::as_str),
        Some("explicit-user")
    );
}

#[test]
fn backfill_login_identity_replaces_an_empty_identity() {
    let Some(expected) = os_login_name() else {
        return;
    };
    let mut vars = BTreeMap::from([("USER".to_string(), String::new())]);

    backfill_login_identity(&mut vars);

    assert_eq!(
        vars.get("USER").map(String::as_str),
        Some(expected.as_str())
    );
}
