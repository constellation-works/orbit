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

#[test]
fn built_in_defaults_are_reachable_without_any_config_file() {
    let global = tempdir().expect("global tempdir");
    let workspace = tempdir().expect("workspace tempdir");

    let resolved =
        ResolvedConfig::load(&roots(global.path(), workspace.path())).expect("built-ins load");

    assert_eq!(resolved.crews, default_crews());
    assert_eq!(
        resolved.snapshot.execution_env_pass,
        ConfigSnapshot::default().execution_env_pass
    );
}

#[test]
fn complexity_crew_pools_are_validated_deduplicated_and_projected() {
    let config = load_config(
        r#"
[workflow]
low_complexity_crews = ["luna"]
medium_complexity_crews = ["terra", " grok ", "terra"]
hard_complexity_crews = ["astra"]
xhard_complexity_crews = ["fable:70", "astra:30"]
"#,
    )
    .expect("valid pools");
    assert_eq!(
        config.complexity_crews.medium,
        Some(vec!["grok".into(), "terra".into()])
    );
    for (key, expected) in [
        ("workflow.low_complexity_crews", serde_json::json!(["luna"])),
        (
            "workflow.medium_complexity_crews",
            serde_json::json!(["grok", "terra"]),
        ),
        (
            "workflow.hard_complexity_crews",
            serde_json::json!(["astra"]),
        ),
        (
            "workflow.xhard_complexity_crews",
            serde_json::json!(["astra:30", "fable:70"]),
        ),
    ] {
        assert_eq!(config.snapshot.value_for(key), Some(expected));
    }
    let weighted = load_config(
        "[workflow]\nmedium_complexity_crews = [\"grok:70\", \"opus:0\", \"terra:30\"]\n",
    )
    .expect("weighted pools load");
    assert_eq!(
        weighted.complexity_crews.medium,
        Some(vec!["grok:70".into(), "opus:0".into(), "terra:30".into()])
    );
    assert_eq!(
        weighted
            .snapshot
            .value_for("workflow.medium_complexity_crews"),
        Some(serde_json::json!(["grok:70", "opus:0", "terra:30"]))
    );
    for invalid in [
        r#"["missing"]"#,
        r#"[" "]"#,
        r#"["grok", ""]"#,
        "[1]",
        "false",
        r#"["grok:50", "terra"]"#,
        r#"["grok:50", "grok:20"]"#,
        r#"["grok:-1"]"#,
        r#"["grok:0", "terra:0"]"#,
    ] {
        let error = load_config(&format!(
            "[workflow]\nmedium_complexity_crews = {invalid}\n"
        ))
        .expect_err("bad pools must fail config admission");
        assert!(
            error.to_string().contains("medium_complexity_crews"),
            "{error}"
        );
    }
}

#[test]
fn complexity_crew_pools_layer_by_replacement_and_empty_disables() {
    let global = tempdir().expect("global");
    let workspace = tempdir().expect("workspace");
    write_config(
        global.path(),
        "[workflow]\nlow_complexity_crews = [\"luna\"]\nmedium_complexity_crews = [\"grok\"]\nhard_complexity_crews = [\"astra\"]\nxhard_complexity_crews = [\"fable\"]\n",
    );
    write_config(
        workspace.path(),
        "[workflow]\nmedium_complexity_crews = [\"terra\"]\nhard_complexity_crews = []\n",
    );
    let config =
        ResolvedConfig::load(&roots(global.path(), workspace.path())).expect("layered pools");
    assert_eq!(config.complexity_crews.low, Some(vec!["luna".into()]));
    assert_eq!(config.complexity_crews.medium, Some(vec!["terra".into()]));
    assert_eq!(config.complexity_crews.xhard, Some(vec!["fable".into()]));
    assert_eq!(config.complexity_crews.hard, Some(vec![]));
    let defaults = load_config("").expect("defaults");
    assert_eq!(defaults.complexity_crews.medium, Some(vec![]));
    assert_eq!(defaults.complexity_crews.xhard, Some(vec![]));
}

/// [ORB-12723] `workflow.pilot_max_complexity` is removed with the pilot
/// ceiling it drove. An existing config that still sets it keeps loading
/// (the loader warns), the key is no registry setting, and `orbit config
/// get`/`set` name the removal rather than offering a did-you-mean.
#[test]
fn removed_pilot_max_complexity_loads_and_is_refused_by_config_get_and_set() {
    let config = load_config(
        r#"
[workflow]
base_branch = "agent-main"
pilot_max_complexity = "xhard"
"#,
    )
    .expect("a config carrying the removed key must load");

    assert_eq!(config.workflow_base_branch, "agent-main");
    assert!(
        config
            .snapshot
            .value_for("workflow.pilot_max_complexity")
            .is_none()
    );
    assert!(crate::describe_config_key("workflow.pilot_max_complexity").is_none());

    let error = crate::admit_config_key("workflow.pilot_max_complexity")
        .expect_err("a removed key is not addressable");
    let message = error.to_string();
    assert!(
        message.contains("workflow.pilot_max_complexity") && message.contains("removed"),
        "{message}"
    );
    assert!(
        !message.contains("unknown config key"),
        "a removed key is reported as removed, not unknown: {message}"
    );
}

/// [ORB-12619] `workflow.*_complexity_crews` reads `:` as the weight
/// separator, so a colon-named crew could be defined but never pooled — every
/// command then failed with a malformed-weight error. The name is refused
/// where the crew is defined instead, for every pool tier alike.
#[test]
fn colon_named_crews_are_refused_at_definition_not_at_every_pool() {
    const CREW: &str = r#"[crews."gpt-5:codex"]
model = "gpt-5.5"
provider = "codex"
"#;

    let error = load_config(CREW).expect_err("a colon-named crew must not be admitted");
    let message = error.to_string();
    assert!(message.contains("[crews]"), "message: {message}");
    assert!(message.contains("gpt-5:codex"), "message: {message}");
    assert!(
        message.contains("workflow.*_complexity_crews"),
        "message: {message}"
    );

    for tier in ["low", "medium", "hard", "xhard"] {
        let body = format!(
            "{CREW}\n[workflow]\ndefault_crew = \"gpt-5:codex\"\n{tier}_complexity_crews = [\"gpt-5:codex\"]\n"
        );
        let error = load_config(&body)
            .err()
            .unwrap_or_else(|| panic!("{tier} pool must refuse the crew name"))
            .to_string();
        assert!(error.contains("[crews]"), "{tier}: {error}");
        assert!(
            error.contains("workflow.*_complexity_crews"),
            "{tier}: {error}"
        );
        assert!(
            !error.contains("must weigh a non-negative whole number"),
            "{tier} must fail at the crew definition, not as a malformed weight: {error}"
        );
    }
}
