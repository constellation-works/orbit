use std::collections::BTreeMap;

use tempfile::tempdir;

use crate::raw::RawRuntimeConfig;
use crate::seed::DEFAULT_CONFIG_TEMPLATE;
use crate::{ConfigRoots, ConfigSeed, CrewSeed, ResolvedConfig, seed_default_config};

fn seed_for(families: &[&str]) -> ConfigSeed {
    ConfigSeed::from_families(families.iter().copied())
}

fn sample_crew_settings() -> BTreeMap<String, CrewSeed> {
    BTreeMap::from([(
        "custom".to_string(),
        CrewSeed {
            provider: Some("codex".into()),
            model: Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.into()),
        },
    )])
}

#[test]
fn default_template_keeps_agent_dependent_sections_out() {
    assert!(!DEFAULT_CONFIG_TEMPLATE.contains("default_crew"));
    assert!(!DEFAULT_CONFIG_TEMPLATE.contains("[crews."));
    assert!(!DEFAULT_CONFIG_TEMPLATE.contains("[duel"));
    assert!(DEFAULT_CONFIG_TEMPLATE.contains("[execution.env]"));
    assert!(DEFAULT_CONFIG_TEMPLATE.contains("[execution.codex]"));
    assert!(DEFAULT_CONFIG_TEMPLATE.contains("[scoring]"));
    assert!(!DEFAULT_CONFIG_TEMPLATE.contains("[graph]"));
    assert!(DEFAULT_CONFIG_TEMPLATE.contains("[workflow]"));
    assert!(DEFAULT_CONFIG_TEMPLATE.contains("base_branch = \"main\""));
}

/// A seed is the only thing that produces crew tables. Without one the file is
/// the static template, so config loading falls back to the built-in crews
/// rather than to an explicitly empty registry.
#[test]
fn no_seed_writes_the_static_template_and_keeps_built_in_crews() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    assert!(seed_default_config(&path, None).expect("seed"));
    let contents = std::fs::read_to_string(&path).expect("read");

    assert!(!contents.contains("[crews"));
    assert!(!contents.contains("default_crew"));
    let resolved = load_seeded_config(&contents);
    assert_eq!(resolved.crews, crate::resolved::default_crews());
    assert_eq!(resolved.default_crew.as_deref(), Some("opus"));
}

#[test]
fn claude_only_seeds_the_claude_family_and_system() {
    let contents = seed_contents(&seed_for(&["claude"]));
    let parsed = parsed_config(&contents);

    assert_eq!(
        crew_names(&parsed),
        vec!["fable", "opus", "sonnet", "system"]
    );
    assert_crew(&parsed, "opus", "claude", "opus");
    assert_crew(&parsed, "sonnet", "claude", "sonnet");
    assert_crew(&parsed, "fable", "claude", "fable");
    assert_crew(&parsed, "system", "claude", "sonnet");
    assert_default_crew(&parsed, Some("opus"));
    assert!(!contents.contains("[duel"));
}

#[test]
fn codex_only_seeds_the_codex_family_and_system() {
    let contents = seed_contents(&seed_for(&["codex"]));
    let parsed = parsed_config(&contents);

    assert_eq!(
        crew_names(&parsed),
        vec!["astra", "luna", "sol", "system", "terra"]
    );
    assert_crew(&parsed, "astra", "codex", "gpt-6-astra");
    assert_crew(&parsed, "sol", "codex", "gpt-5.6-sol");
    assert_crew(&parsed, "terra", "codex", "gpt-5.6-terra");
    assert_crew(&parsed, "luna", "codex", "gpt-5.6-luna");
    assert_crew(&parsed, "system", "codex", "gpt-5.6-luna");
    assert_default_crew(&parsed, Some("astra"));
}

#[test]
fn gemini_only_seeds_gemini_and_a_system_crew() {
    let contents = seed_contents(&seed_for(&["gemini"]));
    let parsed = parsed_config(&contents);

    assert_eq!(crew_names(&parsed), vec!["gemini", "system"]);
    assert_crew(&parsed, "gemini", "gemini", "gemini-3.8-flash");
    assert_crew(&parsed, "system", "gemini", "gemini-3.8-flash");
    assert_default_crew(&parsed, Some("gemini"));
}

#[test]
fn antigravity_only_seeds_antigravity_and_a_system_crew() {
    let contents = seed_contents(&seed_for(&["antigravity"]));
    let parsed = parsed_config(&contents);

    assert_eq!(crew_names(&parsed), vec!["antigravity", "system"]);
    assert_crew(
        &parsed,
        "antigravity",
        "antigravity",
        "gemini-3.8-flash-high",
    );
    assert_crew(&parsed, "system", "antigravity", "gemini-3.8-flash-low");
    assert_default_crew(&parsed, Some("antigravity"));
}

#[test]
fn antigravity_outranks_legacy_gemini_when_both_are_available() {
    let contents = seed_contents(&seed_for(&["antigravity", "gemini"]));
    let parsed = parsed_config(&contents);
    assert_eq!(crew_names(&parsed), vec!["antigravity", "gemini", "system"]);
    assert_default_crew(&parsed, Some("antigravity"));
    assert_crew(&parsed, "system", "antigravity", "gemini-3.8-flash-low");
}

#[test]
fn grok_only_seeds_grok_and_a_system_crew() {
    let contents = seed_contents(&seed_for(&["grok"]));
    let parsed = parsed_config(&contents);

    assert_eq!(crew_names(&parsed), vec!["grok", "system"]);
    assert_crew(&parsed, "grok", "grok", "grok-4.6");
    assert_crew(&parsed, "system", "grok", "grok-4.6");
    assert_default_crew(&parsed, Some("grok"));
}

#[test]
fn cursor_only_seeds_cursor_and_a_system_crew() {
    let contents = seed_contents(&seed_for(&["cursor"]));
    let parsed = parsed_config(&contents);

    assert_eq!(crew_names(&parsed), vec!["cursor", "system"]);
    assert_crew(&parsed, "cursor", "cursor", "gpt-5");
    assert_crew(&parsed, "system", "cursor", "gpt-5");
    assert_default_crew(&parsed, Some("cursor"));
}

#[test]
fn pi_only_seeds_pi_and_a_system_crew() {
    let contents = seed_contents(&seed_for(&["pi"]));
    let parsed = parsed_config(&contents);

    assert_eq!(crew_names(&parsed), vec!["pi", "system"]);
    assert_crew(&parsed, "pi", "pi", "sonnet");
    assert_crew(&parsed, "system", "pi", "sonnet");
    assert_default_crew(&parsed, Some("pi"));
}

/// [ORB-11296] Pi is appended last in the preference order, so installing it
/// beside an earlier family never moves that host's default or system crew.
#[test]
fn adding_pi_never_displaces_an_earlier_family() {
    let parsed = parsed_config(&seed_contents(&seed_for(&["claude", "pi"])));

    assert_default_crew(&parsed, Some("opus"));
    assert_crew(&parsed, "system", "claude", "sonnet");
    assert_crew(&parsed, "pi", "pi", "sonnet");
}

/// Orbit ships no `ollama` crew, so a host whose only agent CLI is ollama
/// seeds an explicitly empty registry rather than a dangling default.
#[test]
fn no_supported_family_seeds_no_crews_or_dangling_default() {
    let contents = seed_contents(&seed_for(&["ollama"]));
    let parsed = parsed_config(&contents);

    assert!(crew_names(&parsed).is_empty());
    assert_default_crew(&parsed, None);
    assert!(!contents.contains("[duel"));
    toml::from_str::<RawRuntimeConfig>(&contents).expect("no-provider config parses");
    let resolved = load_seeded_config(&contents);
    assert!(resolved.crews.is_empty());
    assert_eq!(resolved.default_crew, None);
}

#[test]
fn multi_provider_seed_includes_each_available_family_and_excludes_unavailable() {
    let parsed = parsed_config(&seed_contents(&seed_for(&["claude", "codex", "grok"])));

    assert_eq!(
        crew_names(&parsed),
        vec![
            "astra", "fable", "grok", "luna", "opus", "sol", "sonnet", "system", "terra"
        ]
    );
    assert_default_crew(&parsed, Some("opus"));
    assert_crew(&parsed, "opus", "claude", "opus");
    assert_crew(&parsed, "sonnet", "claude", "sonnet");
    assert_crew(&parsed, "fable", "claude", "fable");
    assert_crew(&parsed, "sol", "codex", "gpt-5.6-sol");
    assert_crew(&parsed, "terra", "codex", "gpt-5.6-terra");
    assert_crew(&parsed, "luna", "codex", "gpt-5.6-luna");
    assert_crew(&parsed, "astra", "codex", "gpt-6-astra");
    assert_crew(&parsed, "grok", "grok", "grok-4.6");
    // codex outranks claude and grok in the system-lane preference order.
    assert_crew(&parsed, "system", "codex", "gpt-5.6-luna");
    for crew in crews(&parsed).values() {
        // [ORB-10801] Seeded crews no longer carry the retired backend key.
        assert!(crew.get("backend").is_none());
        assert_ne!(
            crew.get("provider").and_then(toml::Value::as_str),
            Some("gemini")
        );
    }
    assert!(!crews(&parsed).contains_key("qa"));
}

#[test]
fn seeded_configs_round_trip_for_family_permutations() {
    let cases: [(&str, &[&str]); 6] = [
        ("none", &[]),
        ("one family", &["claude"]),
        ("two families", &["claude", "codex"]),
        ("three families", &["claude", "codex", "gemini"]),
        ("four families", &["claude", "codex", "gemini", "grok"]),
        ("unsupported family only", &["ollama"]),
    ];

    for (name, families) in cases {
        let contents = seed_contents(&seed_for(families));
        assert!(
            !contents.contains("[crews.qa]"),
            "{name} seed must not create the legacy QA crew"
        );
        toml::from_str::<RawRuntimeConfig>(&contents)
            .unwrap_or_else(|err| panic!("{name} raw parse failed: {err}"));
        load_seeded_config(&contents);
    }
}

#[test]
fn seed_with_no_crew_settings_keeps_static_template_content() {
    let contents = seed_contents(&ConfigSeed::default());
    assert!(no_active_role_section(&contents));
    assert!(crew_names(&parsed_config(&contents)).is_empty());
    assert!(!contents.contains("default_crew"));
    assert!(contents.contains("sandbox = \"danger-full-access\""));
}

fn seed_contents(seed: &ConfigSeed) -> String {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    let created = seed_default_config(&path, Some(seed)).expect("seed");
    assert!(created);
    std::fs::read_to_string(&path).expect("read")
}

fn load_seeded_config(contents: &str) -> ResolvedConfig {
    let dir = tempdir().expect("tempdir");
    std::fs::write(dir.path().join("config.toml"), contents).expect("write config");
    ResolvedConfig::load(&ConfigRoots::global_only(dir.path())).expect("resolved config loads")
}

fn parsed_config(contents: &str) -> toml::Value {
    toml::from_str(contents).expect("parse seeded config")
}

fn crews(parsed: &toml::Value) -> &toml::map::Map<String, toml::Value> {
    parsed
        .get("crews")
        .and_then(toml::Value::as_table)
        .unwrap_or_else(|| empty_toml_table())
}

fn empty_toml_table() -> &'static toml::map::Map<String, toml::Value> {
    static EMPTY: std::sync::OnceLock<toml::map::Map<String, toml::Value>> =
        std::sync::OnceLock::new();
    EMPTY.get_or_init(toml::map::Map::new)
}

fn crew_names(parsed: &toml::Value) -> Vec<&str> {
    crews(parsed).keys().map(String::as_str).collect()
}

fn assert_crew(parsed: &toml::Value, name: &str, provider: &str, model: &str) {
    let crew = crews(parsed).get(name).expect("expected crew");
    assert_eq!(
        crew.get("provider").and_then(toml::Value::as_str),
        Some(provider)
    );
    assert_eq!(crew.get("model").and_then(toml::Value::as_str), Some(model));
    assert!(crew.get("backend").is_none());
}

fn assert_default_crew(parsed: &toml::Value, expected: Option<&str>) {
    assert_eq!(
        parsed
            .get("workflow")
            .and_then(|workflow| workflow.get("default_crew"))
            .and_then(toml::Value::as_str),
        expected,
    );
}

fn no_active_role_section(contents: &str) -> bool {
    contents
        .lines()
        .all(|line| !line.trim_start().starts_with("[agent."))
}

#[test]
fn seed_with_crew_settings_writes_custom_crew() {
    let contents = seed_contents(&ConfigSeed::default().with_crews(sample_crew_settings()));

    assert!(no_active_role_section(&contents));
    assert!(contents.contains("default_crew = \"custom\""));
    assert!(contents.contains("[crews.custom]"));
    assert!(contents.contains("provider = \"codex\""));
    assert!(contents.contains(&format!(
        "model = \"{}\"",
        orbit_common::test_fixtures::TEST_CODEX_MODEL
    )));

    // Round-trips through toml::from_str (consumer side will need this).
    let parsed: toml::Value = toml::from_str(&contents).expect("parse");
    let crews = parsed
        .get("crews")
        .expect("crews table")
        .as_table()
        .expect("crews is a table");
    assert_eq!(crews.len(), 1, "custom init must not invent provider crews");
    let custom = crews
        .get("custom")
        .and_then(|v| v.as_table())
        .expect("custom crew");
    assert_eq!(
        custom.get("provider").and_then(|v| v.as_str()),
        Some("codex")
    );
    assert!(custom.get("backend").is_none());
    assert_eq!(
        custom.get("model").and_then(|v| v.as_str()),
        Some(orbit_common::test_fixtures::TEST_CODEX_MODEL)
    );
}

#[test]
fn seed_with_existing_file_is_noop() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "# pre-existing user content\n").expect("preseed");

    let seed = ConfigSeed::default().with_crews(sample_crew_settings());
    let created = seed_default_config(&path, Some(&seed)).expect("seed");
    assert!(!created);

    let contents = std::fs::read_to_string(&path).expect("read");
    assert_eq!(contents, "# pre-existing user content\n");
}

#[test]
fn seed_with_empty_crew_map_uses_no_provider_behavior() {
    let contents = seed_contents(&ConfigSeed::default().with_crews(BTreeMap::new()));
    let parsed = parsed_config(&contents);
    assert!(crew_names(&parsed).is_empty());
    assert_default_crew(&parsed, None);
}

#[test]
fn seed_with_incomplete_crew_settings_fails() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    let mut settings = sample_crew_settings();
    settings.get_mut("custom").expect("custom").model.take();
    let seed = ConfigSeed::default().with_crews(settings);

    let error = seed_default_config(&path, Some(&seed)).expect_err("missing model fails");
    assert!(
        error
            .to_string()
            .contains("custom crew is missing required `model`")
    );
    assert!(!path.exists());
}
