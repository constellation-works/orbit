//! The `name[:weight]` pool grammar, admitted once for configuration,
//! `orbit config set` and CLI overrides alike [ORB-12604].

use crate::canonical_crew_pool;
use orbit_types::identity::{Crew, CrewAssignment};
use std::collections::BTreeMap;

const SETTING: &str = "workflow.medium_complexity_crews";

fn crews() -> BTreeMap<String, Crew> {
    ["grok", "opus", "sol", "terra"]
        .into_iter()
        .map(|name| {
            (
                name.to_string(),
                Crew {
                    name: name.to_string(),
                    assignment: CrewAssignment {
                        model: format!("{name}-model"),
                        provider: "claude".to_string(),
                        effort: None,
                    },
                    description: None,
                    tags: Vec::new(),
                    enabled: true,
                },
            )
        })
        .collect()
}

fn pool(entries: &[&str]) -> Result<Vec<String>, String> {
    let entries: Vec<String> = entries.iter().map(ToString::to_string).collect();
    canonical_crew_pool(&entries, &crews(), SETTING)
        .map(|pool| pool.to_setting_value())
        .map_err(|error| error.to_string())
}

#[test]
fn malformed_pools_name_the_setting_they_came_from() {
    for (entries, expected) in [
        (vec!["grok:50", "terra"], "all bare crew names"),
        (vec!["grok", "terra:50"], "all bare crew names"),
        (vec!["grok:50", "grok:20"], "more than once"),
        (vec!["grok:-1"], "non-negative whole number"),
        (vec!["grok:2.5"], "non-negative whole number"),
        (vec!["grok:"], "non-negative whole number"),
        (vec!["grok:70", "terra:"], "non-negative whole number"),
        (vec!["grok:0", "terra:0"], "weight above 0"),
        (vec![":50"], "non-empty crew names"),
        (vec!["missing:50"], "is not defined in [crews.*]"),
    ] {
        let error = pool(&entries).expect_err(&format!("{entries:?} must be rejected"));
        assert!(error.contains(SETTING), "{error}");
        assert!(error.contains(expected), "{error}");
    }
}

/// `workflow.final_recovery_crews` as one load admits it from `config.toml`.
fn final_recovery_pool(body: &str) -> Result<Vec<String>, String> {
    let global = tempfile::tempdir().expect("global tempdir");
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    super::write_config(workspace.path(), body);
    crate::ResolvedConfig::load(&super::roots(global.path(), workspace.path()))
        .map(|config| config.snapshot.final_recovery_crews().to_vec())
        .map_err(|error| error.to_string())
}

#[test]
fn final_recovery_pool_defaults_to_sol_and_opus_and_shows_in_config() {
    let pool = final_recovery_pool("").expect("built-in registry admits the default pool");
    assert_eq!(pool, ["opus:20", "sol:100"]);
    let built_in = crate::ConfigSnapshot::default();
    assert_eq!(
        built_in.value_for("workflow.final_recovery_crews"),
        Some(serde_json::json!(["opus:20", "sol:100"])),
        "`orbit config show` projects the admitted default"
    );
}

#[test]
fn final_recovery_pool_admits_weighted_pools_and_empty_disables_it() {
    let body = |pool: &str| {
        format!(
            "[workflow]\ndefault_crew = \"sol\"\nfinal_recovery_crews = {pool}\n\n\
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
    for (pool, expected) in [
        (r#"["sol:3", "grok"]"#, "all bare crew names"),
        (r#"["opus:20"]"#, "is not defined in [crews.*]"),
        (r#"["sol:0"]"#, "weight above 0"),
    ] {
        let error = final_recovery_pool(&body(pool)).expect_err(pool);
        assert!(error.contains("workflow.final_recovery_crews"), "{error}");
        assert!(error.contains(expected), "{error}");
    }
}

#[test]
fn unset_final_recovery_pool_keeps_only_crews_a_custom_registry_defines() {
    let opus_only = "[crews.opus]\nmodel = \"m\"\nprovider = \"claude\"\n";
    assert_eq!(
        final_recovery_pool(opus_only).expect("partial default"),
        ["opus:20"]
    );
    let neither = "[workflow]\ndefault_crew = \"grok\"\n\n\
                   [crews.grok]\nmodel = \"m\"\nprovider = \"grok\"\n";
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
    super::write_config(
        workspace.path(),
        "[workflow]\ndefault_crew = \"terra\"\nmedium_complexity_crews = [\"terra\"]\n\n\
         [crews.terra]\nmodel = \"gpt-5.6-terra\"\nprovider = \"codex\"\n",
    );

    let config = crate::ResolvedConfig::load(&super::roots(global.path(), workspace.path()))
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
