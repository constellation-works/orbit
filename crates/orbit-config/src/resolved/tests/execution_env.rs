use super::*;

/// The ambient environment an operator would reasonably expect
/// `inherit = false` to exclude: benignly named credentials and service URLs
/// alongside the runtime context a provider CLI genuinely needs. Stated by the
/// test so the assertions never depend on the developer's shell. [ORB-10917]
fn ambient_env_with_credentials() -> orbit_common::test_env::ScopedEnv {
    orbit_common::test_env::scoped([
        ("DATABASE_URL", Some("postgres://svc:hunter2@db.internal")),
        ("BILLING_ENDPOINT", Some("https://billing.internal.example")),
        ("ANTHROPIC_API_KEY", Some("sk-ant-00000000000000000000")),
        ("HOME", Some("/home/agent")),
        ("PATH", Some("/usr/bin:/bin")),
        ("CODEX_HOME", Some("/home/agent/.codex")),
        ("ORBIT_RUN_ID", Some("jrun-10917")),
    ])
}

fn value_of<'a>(env: &'a [(String, String)], key: &str) -> Option<&'a str> {
    env.iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.as_str())
}

#[test]
fn default_policy_excludes_benignly_named_ambient_credentials() {
    let _ambient = ambient_env_with_credentials();
    let policy = ExecutionEnvPolicy::default();

    let env = policy.agent_subprocess_env(&[]);

    assert_eq!(value_of(&env, "DATABASE_URL"), None);
    assert_eq!(value_of(&env, "BILLING_ENDPOINT"), None);
    assert_eq!(value_of(&env, "ANTHROPIC_API_KEY"), None);
    // The documented baseline, the configured pass list, and the ORBIT_*
    // execution envelope still reach the child.
    assert_eq!(value_of(&env, "HOME"), Some("/home/agent"));
    assert_eq!(value_of(&env, "PATH"), Some("/usr/bin:/bin"));
    assert_eq!(value_of(&env, "CODEX_HOME"), Some("/home/agent/.codex"));
    assert_eq!(value_of(&env, "ORBIT_RUN_ID"), Some("jrun-10917"));
}

#[test]
fn configured_pass_list_is_the_admission_path_for_an_ambient_credential() {
    let _ambient = ambient_env_with_credentials();
    let policy = ExecutionEnvPolicy {
        inherit: false,
        pass: vec!["DATABASE_URL".to_string()],
    };

    let env = policy.agent_subprocess_env(&[]);

    assert_eq!(
        value_of(&env, "DATABASE_URL"),
        Some("postgres://svc:hunter2@db.internal")
    );
    assert_eq!(value_of(&env, "BILLING_ENDPOINT"), None);
}

#[test]
fn provider_required_extras_reach_the_child_under_a_narrow_pass_list() {
    let _ambient = ambient_env_with_credentials();
    let policy = ExecutionEnvPolicy {
        inherit: false,
        pass: Vec::new(),
    };

    let env = policy.agent_subprocess_env(&["CODEX_HOME"]);

    assert_eq!(value_of(&env, "CODEX_HOME"), Some("/home/agent/.codex"));
    assert_eq!(value_of(&env, "DATABASE_URL"), None);
}

/// `inherit = true` stays a real, explicit opt-in to full inheritance. The
/// config surface pins the flag off (see `ExecutionEnvPolicy::inherit`), so the
/// branch is asserted against a directly constructed policy.
#[test]
fn inherit_opts_into_full_environment_inheritance() {
    let _ambient = ambient_env_with_credentials();
    let policy = ExecutionEnvPolicy {
        inherit: true,
        pass: Vec::new(),
    };

    let env = policy.agent_subprocess_env(&[]);

    assert_eq!(
        value_of(&env, "DATABASE_URL"),
        Some("postgres://svc:hunter2@db.internal"),
        "inherit = true forwards the whole parent environment by design"
    );
    assert_eq!(
        value_of(&env, "ANTHROPIC_API_KEY"),
        Some("sk-ant-00000000000000000000")
    );
}

#[test]
fn config_admission_pins_inherit_off() {
    let global = tempdir().expect("global tempdir");
    let workspace = tempdir().expect("workspace tempdir");
    write_config(
        workspace.path(),
        "[execution.env]\ninherit = true\npass = [\"HOME\"]\n",
    );

    let resolved =
        ResolvedConfig::load(&roots(global.path(), workspace.path())).expect("config loads");

    assert!(
        !resolved.execution_env.inherit(),
        "execution.env.inherit is inert; a config file must not re-enable inheritance"
    );
}
