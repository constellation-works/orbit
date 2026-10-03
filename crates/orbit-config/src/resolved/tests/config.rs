use super::*;

#[test]
fn pr_config_defaults_to_no_task_url_template_without_config() {
    let global = tempdir().expect("global tempdir");
    let workspace = tempdir().expect("workspace tempdir");

    let config =
        ResolvedConfig::load(&roots(global.path(), workspace.path())).expect("config loads");

    assert_eq!(config.pr.task_url_template.as_deref(), None);
}

#[test]
fn pr_task_url_template_loads_from_workspace_config() {
    let config =
        load_config("[pr]\ntask_url_template = \"https://orbit-cli.com/tasks/{task_id}\"\n")
            .expect("config loads");

    assert_eq!(
        config.pr.task_url_template.as_deref(),
        Some("https://orbit-cli.com/tasks/{task_id}")
    );
}

#[test]
fn workflow_auto_ship_defaults_false_and_loads_when_set() {
    let global = tempdir().expect("global tempdir");
    let workspace = tempdir().expect("workspace tempdir");

    write_config(workspace.path(), "");
    let config =
        ResolvedConfig::load(&roots(global.path(), workspace.path())).expect("config loads");
    assert!(!config.workflow_auto_ship);

    write_config(
        workspace.path(),
        r#"
[workflow]
auto_ship = true
"#,
    );
    let config =
        ResolvedConfig::load(&roots(global.path(), workspace.path())).expect("config loads");
    assert!(config.workflow_auto_ship);
}

#[test]
fn shipped_default_config_has_no_workspace_specific_identifiers() {
    let shipped = include_str!("../../../assets/default-config.toml");

    for forbidden in ["ORB-", "agent-main", "dk-server", "F2026-"] {
        assert!(
            !shipped.contains(forbidden),
            "seeded default config must not leak workspace-specific marker {forbidden:?} into every consumer's config"
        );
    }
}

/// The seeded config documents `[workflow.task_pilot_freshness]` only as a
/// comment: uncommenting its keys must reproduce the built-in defaults, and a
/// fresh seed must leave both keys at those defaults with the seeded crew keys
/// still inside `[workflow]`.
#[test]
fn seeded_task_pilot_freshness_comment_matches_built_in_defaults() {
    let shipped = crate::seed::DEFAULT_CONFIG_TEMPLATE;
    let keys = ["material_fields", "source_sensitivity"];
    let uncommented = shipped
        .lines()
        .filter_map(|line| line.strip_prefix("# "))
        .filter(|line| keys.iter().any(|key| line.starts_with(&format!("{key} "))))
        .collect::<Vec<_>>();
    assert_eq!(uncommented.len(), keys.len(), "{uncommented:?}");
    let documented = load_config(&format!(
        "[workflow.task_pilot_freshness]\n{}\n",
        uncommented.join("\n")
    ))
    .expect("the documented freshness block must load");
    let built_in = load_config("").expect("an empty config loads");
    assert_eq!(
        documented.snapshot.task_pilot_freshness(),
        built_in.snapshot.task_pilot_freshness(),
        "the commented freshness block in default-config.toml drifted from settings.rs"
    );

    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    let seed = crate::ConfigSeed::from_families(["claude"]);
    assert!(crate::seed_default_config(&path, Some(&seed)).expect("seed"));
    let effective = crate::load_effective_config(&crate::ConfigRoots::global_only(dir.path()))
        .expect("seeded config loads");
    for key in keys {
        let key = format!("workflow.task_pilot_freshness.{key}");
        let entry = effective
            .values()
            .iter()
            .find(|entry| entry.key == key)
            .unwrap_or_else(|| panic!("{key} is reported"));
        assert_eq!(entry.state(), crate::ConfigValueState::Default, "{key}");
    }
    assert_eq!(
        effective.value_for("workflow.default_crew"),
        Some(serde_json::json!("opus"))
    );
}

#[test]
fn runtime_log_rotation_rejects_invalid_values() {
    // [ORB-00415] Malformed rotation knobs must fail at config load with a
    // clear, key-naming error.
    let error = load_config("[runtime]\nlog_retention_days = 0\n")
        .expect_err("zero retention must fail config load");
    assert!(
        error.to_string().contains("log_retention_days"),
        "message: {error}"
    );

    let error = load_config("[runtime]\nlog_max_total_mb = 10\nlog_max_file_mb = 50\n")
        .expect_err("per-file budget above total must fail config load");
    assert!(
        error.to_string().contains("log_max_file_mb"),
        "message: {error}"
    );
}

#[test]
fn runtime_log_rotation_accepts_valid_values() {
    load_config(
        "[runtime]\nlog_retention_days = 14\nlog_max_total_mb = 200\nlog_max_file_mb = 20\n",
    )
    .expect("valid log rotation config should load");
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
