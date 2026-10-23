use super::default_crew_write_config;
use orbit_config::ConfigRoots;

/// `workflow.purpose_crews.final_recovery` as one load admits it from `config.toml`.
fn final_recovery_pool(body: &str) -> Result<Vec<String>, String> {
    let global = tempfile::tempdir().expect("global tempdir");
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    default_crew_write_config(workspace.path(), body);
    orbit_config::ResolvedConfig::load(&ConfigRoots::new(global.path(), workspace.path()))
        .map(|config| config.snapshot.final_recovery_crews().to_vec())
        .map_err(|error| error.to_string())
}

#[test]
fn final_recovery_pool_admits_weighted_pools_and_empty_disables_it() {
    let body = |pool: &str| {
        format!(
            "[workflow]\ndefault_crew = \"sol\"\n\n[workflow.purpose_crews]\nfinal_recovery = {pool}\n\n\
             [crews.sol]\nmodel = \"m\"\nprovider = \"codex\"\n\n\
             [crews.grok]\nmodel = \"m\"\nprovider = \"grok\"\n"
        )
    };
    assert_eq!(
        final_recovery_pool(&body(r#"["sol:3", "grok:1"]"#)).expect("weighted pool"),
        ["grok:1", "sol:3"]
    );
    assert_eq!(
        final_recovery_pool(&body(r#"["sol"]"#)).expect("bare pool"),
        ["sol"]
    );
    assert!(
        final_recovery_pool(&body("[]"))
            .expect("empty pool")
            .is_empty()
    );
    // The deprecated key loads into the same pool.
    assert_eq!(
        final_recovery_pool(
            "[workflow]\ndefault_crew = \"sol\"\nfinal_recovery_crews = [\"sol\"]\n\n\
             [crews.sol]\nmodel = \"m\"\nprovider = \"codex\"\n"
        )
        .expect("deprecated key"),
        ["sol"]
    );
    for (pool, expected) in [
        (r#"["sol:3", "grok"]"#, "all bare crew names"),
        (r#"["opus:20"]"#, "is not defined in [crews.*]"),
        (r#"["sol:0"]"#, "weight above 0"),
    ] {
        let error = final_recovery_pool(&body(pool)).expect_err(pool);
        assert!(
            error.contains("workflow.purpose_crews.final_recovery"),
            "{error}"
        );
        assert!(error.contains(expected), "{error}");
    }
}

#[test]
fn unset_final_recovery_pool_keeps_only_crews_a_custom_registry_defines() {
    let defaults = final_recovery_pool("").expect("default pool");
    let entry = defaults.first().expect("non-empty default pool");
    let name = entry.split(':').next().expect("entry has a crew name");
    let name = toml::Value::String(name.to_string());
    let one_member = format!(
        "[workflow]\ndefault_crew = {name}\n\n[crews.{name}]\nmodel = \"fixture-model\"\nprovider = \"codex\"\n"
    );
    assert_eq!(
        final_recovery_pool(&one_member).expect("partial default"),
        std::slice::from_ref(entry)
    );
    let neither = "[workflow]\ndefault_crew = \"fixture-only\"\n\n\
                   [crews.fixture-only]\nmodel = \"m\"\nprovider = \"codex\"\n";
    assert!(
        final_recovery_pool(neither)
            .expect("a registry without the default crews still loads")
            .is_empty(),
        "no default member defined leaves final recovery disabled"
    );
}

#[test]
fn user_authored_terra_crew_still_loads_and_resolves_in_complexity_pool() {
    let global = tempfile::tempdir().expect("global tempdir");
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    default_crew_write_config(
        workspace.path(),
        "[workflow]\ndefault_crew = \"terra\"\nmedium_complexity_crews = [\"terra\"]\n\n\
         [crews.terra]\nmodel = \"gpt-5.6-terra\"\nprovider = \"codex\"\n",
    );

    let config =
        orbit_config::ResolvedConfig::load(&ConfigRoots::new(global.path(), workspace.path()))
            .expect("user-authored Terra crew remains an ordinary configured crew");

    assert_eq!(config.crews["terra"].assignment.model, "gpt-5.6-terra");
    assert_eq!(config.crews["terra"].assignment.provider, "codex");
    assert_eq!(
        config
            .snapshot
            .value_for("workflow.medium_complexity_crews"),
        Some(serde_json::json!(["terra"]))
    );
}
