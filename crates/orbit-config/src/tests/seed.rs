use tempfile::tempdir;

use crate::raw::RawRuntimeConfig;
use crate::seed::DEFAULT_CONFIG_TEMPLATE;
use crate::{ConfigRoots, ConfigSeed, ResolvedConfig, seed_default_config};

fn seed_for(families: &[&str]) -> ConfigSeed {
    ConfigSeed::from_families(families.iter().copied())
}

/// Every built-in crew a seed writes, whatever was detected. The `system`
/// alias is never a seeded table.
const ALL_SEEDED_CREWS: [&str; 14] = [
    "antigravity",
    "astra",
    "copilot",
    "cursor",
    "fable",
    "gemini",
    "grok",
    "luna",
    "opencode",
    "opus",
    "pi",
    "sol",
    "sonnet",
    "terra",
];

const POOL_KEYS: [&str; 4] = [
    "low_complexity_crews",
    "medium_complexity_crews",
    "hard_complexity_crews",
    "xhard_complexity_crews",
];

/// The static template leaves every agent-dependent choice to the seed: it
/// defines no crew tables and names no crew or pool, and carries no retired
/// section.
#[test]
fn default_template_keeps_agent_dependent_sections_out() {
    let parsed = parsed_config(DEFAULT_CONFIG_TEMPLATE);

    assert!(crew_names(&parsed).is_empty());
    for key in ["default_crew", "system_crew"].into_iter().chain(POOL_KEYS) {
        assert!(
            workflow_value(&parsed, key).is_none(),
            "workflow.{key} is written only by a seed"
        );
    }
    assert!(parsed.get("duel").is_none());
    assert!(parsed.get("graph").is_none());

    let resolved = load_seeded_config(DEFAULT_CONFIG_TEMPLATE);
    assert_eq!(resolved.workflow_base_branch, "main");
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

    assert_eq!(
        parsed_config(&contents),
        parsed_config(DEFAULT_CONFIG_TEMPLATE)
    );
    let resolved = load_seeded_config(&contents);
    assert_eq!(resolved.crews, crate::resolved::default_crews());
    assert_eq!(resolved.default_crew.as_deref(), Some("opus"));
    assert_eq!(resolved.system_crew, "system");
}

/// [ORB-12719] The shape a claude + codex host seeds non-interactively:
/// `default_crew` and `system_crew` name real built-in crews, the four pools
/// are scaffolded empty, and no `custom` or `system` crew table exists.
#[test]
fn claude_and_codex_seed_names_real_crews_and_empty_pools() {
    let contents = seed_contents(&seed_for(&["claude", "codex"]));
    let parsed = parsed_config(&contents);

    assert_eq!(crew_names(&parsed), ALL_SEEDED_CREWS);
    assert_eq!(
        enabled_crew_names(&parsed),
        vec!["astra", "fable", "luna", "opus", "sol", "sonnet", "terra"]
    );
    assert_workflow_str(&parsed, "default_crew", Some("opus"));
    assert_workflow_str(&parsed, "system_crew", Some("luna"));
    assert_empty_pools(&parsed);

    let resolved = load_seeded_config(&contents);
    assert_eq!(resolved.default_crew.as_deref(), Some("opus"));
    assert_eq!(resolved.system_crew, "luna");
    for key in POOL_KEYS {
        assert_eq!(
            resolved.snapshot.value_for(&format!("workflow.{key}")),
            Some(serde_json::json!([])),
            "workflow.{key} must admit as an empty pool"
        );
    }
    let pools = &resolved.complexity_crews;
    for pool in [&pools.low, &pools.medium, &pools.hard, &pools.xhard] {
        assert_eq!(pool.as_deref(), Some(&[][..]));
    }
}

/// A Claude-only host writes every built-in crew and enables exactly the
/// Claude ones; the rest load as defined-but-disabled crews.
#[test]
fn claude_only_seeds_every_crew_and_enables_the_claude_family() {
    let contents = seed_contents(&seed_for(&["claude"]));
    let parsed = parsed_config(&contents);

    assert_eq!(crew_names(&parsed), ALL_SEEDED_CREWS);
    assert_eq!(enabled_crew_names(&parsed), vec!["fable", "opus", "sonnet"]);
    let resolved = load_seeded_config(&contents);
    let enabled = resolved
        .crews
        .values()
        .filter(|crew| crew.enabled)
        .map(|crew| crew.name.as_str())
        .collect::<Vec<_>>();
    // `system` mirrors the enabled `sonnet` crew.
    assert_eq!(enabled, vec!["fable", "opus", "sonnet", "system"]);
    assert!(
        resolved
            .crews
            .values()
            .all(|crew| crew.enabled == (crew.assignment.provider == "claude")),
        "exactly the Claude crews load enabled"
    );
    assert!(resolved.disabled_lane_crews().is_empty());
    assert_crew(&parsed, "opus", "claude", "opus");
    assert_crew(&parsed, "sonnet", "claude", "sonnet");
    assert_crew(&parsed, "fable", "claude", "fable");
    assert_workflow_str(&parsed, "default_crew", Some("opus"));
    assert_workflow_str(&parsed, "system_crew", Some("sonnet"));
    assert!(!contents.contains("[duel"));
}

#[test]
fn codex_only_seeds_the_codex_family() {
    let contents = seed_contents(&seed_for(&["codex"]));
    let parsed = parsed_config(&contents);

    assert_eq!(
        enabled_crew_names(&parsed),
        vec!["astra", "luna", "sol", "terra"]
    );
    assert_crew(&parsed, "astra", "codex", "gpt-6-astra");
    assert_crew(&parsed, "sol", "codex", "gpt-6-sol");
    assert_crew(&parsed, "terra", "codex", "gpt-5.6-terra");
    assert_crew(&parsed, "luna", "codex", "gpt-6-luna");
    assert_workflow_str(&parsed, "default_crew", Some("astra"));
    assert_workflow_str(&parsed, "system_crew", Some("luna"));
}

/// Single-crew families name that one crew for both lanes.
#[test]
fn single_crew_families_name_their_crew_for_both_lanes() {
    for (family, model) in [
        ("gemini", "gemini-3.8-flash"),
        ("antigravity", "gemini-3.8-flash-high"),
        ("grok", "grok-4.7"),
        ("cursor", "gpt-5"),
        ("pi", "sonnet"),
        ("opencode", "anthropic/claude-sonnet-4-5"),
    ] {
        let contents = seed_contents(&seed_for(&[family]));
        let parsed = parsed_config(&contents);

        assert_eq!(enabled_crew_names(&parsed), vec![family], "{family}");
        assert_crew(&parsed, family, family, model);
        assert_workflow_str(&parsed, "default_crew", Some(family));
        assert_workflow_str(&parsed, "system_crew", Some(family));
        let resolved = load_seeded_config(&contents);
        assert_eq!(
            resolved
                .crews
                .get("system")
                .map(|crew| crew.assignment.model.as_str()),
            Some(model),
            "{family}: `system` must alias the named system crew"
        );
    }
}

#[test]
fn antigravity_outranks_legacy_gemini_when_both_are_available() {
    let parsed = parsed_config(&seed_contents(&seed_for(&["antigravity", "gemini"])));
    assert_eq!(enabled_crew_names(&parsed), vec!["antigravity", "gemini"]);
    assert_workflow_str(&parsed, "default_crew", Some("antigravity"));
    assert_workflow_str(&parsed, "system_crew", Some("antigravity"));
}

/// [ORB-11296] [ORB-11295] Pi and OpenCode are appended last in the preference
/// order, so installing them beside an earlier family never moves that host's
/// default or system crew.
#[test]
fn appended_families_never_displace_an_earlier_family() {
    let parsed = parsed_config(&seed_contents(&seed_for(&["claude", "pi", "opencode"])));

    assert_workflow_str(&parsed, "default_crew", Some("opus"));
    assert_workflow_str(&parsed, "system_crew", Some("sonnet"));
    assert_crew(&parsed, "pi", "pi", "sonnet");
    assert_crew(
        &parsed,
        "opencode",
        "opencode",
        "anthropic/claude-sonnet-4-5",
    );
}

/// Orbit ships no `ollama` crew, so a host whose only agent CLI is ollama
/// seeds every crew disabled and names no lane crew. The built-in default
/// still resolves by name, but to a disabled crew, so dispatch refuses rather
/// than silently running a provider this host never detected.
#[test]
fn no_supported_family_seeds_every_crew_disabled() {
    let contents = seed_contents(&seed_for(&["ollama"]));
    let parsed = parsed_config(&contents);

    assert_eq!(crew_names(&parsed), ALL_SEEDED_CREWS);
    assert!(enabled_crew_names(&parsed).is_empty());
    assert_workflow_str(&parsed, "default_crew", None);
    assert_workflow_str(&parsed, "system_crew", None);
    assert_empty_pools(&parsed);
    assert!(!contents.contains("[duel"));
    toml::from_str::<RawRuntimeConfig>(&contents).expect("no-provider config parses");
    let resolved = load_seeded_config(&contents);
    assert!(resolved.crews.values().all(|crew| !crew.enabled));
    assert_eq!(resolved.default_crew.as_deref(), Some("opus"));
    let lanes = resolved
        .disabled_lane_crews()
        .into_iter()
        .map(|lane| (lane.key, lane.crew, lane.enable_target))
        .collect::<Vec<_>>();
    assert_eq!(
        lanes,
        vec![
            (
                "workflow.default_crew",
                "opus".to_string(),
                "opus".to_string()
            ),
            (
                "workflow.system_crew",
                "system".to_string(),
                "opus".to_string()
            ),
        ],
        "both lanes resolve to a disabled crew and name the table that enables it"
    );
}

#[test]
fn multi_provider_seed_includes_each_available_family_and_excludes_unavailable() {
    let parsed = parsed_config(&seed_contents(&seed_for(&["claude", "codex", "grok"])));

    assert_eq!(crew_names(&parsed), ALL_SEEDED_CREWS);
    assert_eq!(
        enabled_crew_names(&parsed),
        vec![
            "astra", "fable", "grok", "luna", "opus", "sol", "sonnet", "terra"
        ]
    );
    assert_workflow_str(&parsed, "default_crew", Some("opus"));
    // codex outranks claude and grok in the system-lane preference order.
    assert_workflow_str(&parsed, "system_crew", Some("luna"));
    for crew in crews(&parsed).values() {
        // [ORB-10801] Seeded crews no longer carry the retired backend key.
        assert!(crew.get("backend").is_none());
        if crew.get("provider").and_then(toml::Value::as_str) == Some("gemini") {
            assert_eq!(crew.get("enabled"), Some(&toml::Value::Boolean(false)));
        }
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
        for retired in ["[crews.qa]", "[crews.custom]", "[crews.system]"] {
            assert!(
                !contents.contains(retired),
                "{name} seed must not create {retired}"
            );
        }
        toml::from_str::<RawRuntimeConfig>(&contents)
            .unwrap_or_else(|err| panic!("{name} raw parse failed: {err}"));
        load_seeded_config(&contents);
    }
}

#[test]
fn seed_with_no_families_keeps_static_template_content() {
    let contents = seed_contents(&ConfigSeed::default());
    assert!(no_active_role_section(&contents));
    let parsed = parsed_config(&contents);
    assert!(enabled_crew_names(&parsed).is_empty());
    assert!(!contents.contains("default_crew ="));
    assert_empty_pools(&parsed);
    assert!(contents.contains("sandbox = \"danger-full-access\""));
}

/// Operator choices are written by name, exactly as chosen, and the seed
/// offers only crews it enables.
#[test]
fn chosen_crews_are_written_by_name() {
    let seed = seed_for(&["claude", "codex"])
        .with_default_crew("astra")
        .with_system_crew("sonnet");
    assert_eq!(seed.recommended_default_crew(), Some("opus"));
    assert_eq!(seed.recommended_system_crew(), Some("luna"));
    assert_eq!(seed.system_crew_options(), vec!["luna", "sonnet"]);
    assert_eq!(
        seed.enabled_crews()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        vec!["astra", "fable", "luna", "opus", "sol", "sonnet", "terra"]
    );

    let contents = seed_contents(&seed);
    let parsed = parsed_config(&contents);
    assert_workflow_str(&parsed, "default_crew", Some("astra"));
    assert_workflow_str(&parsed, "system_crew", Some("sonnet"));
    assert!(!contents.contains("[crews.custom]"));
    let resolved = load_seeded_config(&contents);
    assert_eq!(resolved.default_crew.as_deref(), Some("astra"));
    assert_eq!(
        resolved
            .crews
            .get("system")
            .map(|crew| crew.assignment.model.as_str()),
        Some("sonnet")
    );
}

#[test]
fn a_chosen_crew_the_host_does_not_enable_is_refused_before_writing() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");

    let seed = seed_for(&["claude"]).with_default_crew("luna");
    let error = seed_default_config(&path, Some(&seed)).expect_err("unseeded default crew fails");
    assert!(
        error.to_string().contains(
            "workflow.default_crew names crew `luna`, which this host does not seed enabled"
        ),
        "{error}"
    );
    assert!(!path.exists());

    let seed = seed_for(&["claude"]).with_system_crew("custom");
    let error = seed_default_config(&path, Some(&seed)).expect_err("unseeded system crew fails");
    assert!(
        error.to_string().contains("workflow.system_crew"),
        "{error}"
    );
    assert!(!path.exists());
}

#[test]
fn seed_with_existing_file_is_noop() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "# pre-existing user content\n").expect("preseed");

    let seed = seed_for(&["claude"]);
    let created = seed_default_config(&path, Some(&seed)).expect("seed");
    assert!(!created);

    let contents = std::fs::read_to_string(&path).expect("read");
    assert_eq!(contents, "# pre-existing user content\n");
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

/// Seeded crews whose table says `enabled = true`. Every seeded table writes
/// the key explicitly, so a missing one fails here rather than defaulting.
fn enabled_crew_names(parsed: &toml::Value) -> Vec<&str> {
    crews(parsed)
        .iter()
        .filter(|(name, crew)| {
            crew.get("enabled")
                .and_then(toml::Value::as_bool)
                .unwrap_or_else(|| panic!("seeded [crews.{name}] must write `enabled`"))
        })
        .map(|(name, _)| name.as_str())
        .collect()
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

fn workflow_value<'a>(parsed: &'a toml::Value, key: &str) -> Option<&'a toml::Value> {
    parsed
        .get("workflow")
        .and_then(|workflow| workflow.get(key))
}

fn assert_workflow_str(parsed: &toml::Value, key: &str, expected: Option<&str>) {
    assert_eq!(
        workflow_value(parsed, key).and_then(toml::Value::as_str),
        expected,
        "workflow.{key}"
    );
}

fn assert_empty_pools(parsed: &toml::Value) {
    for key in POOL_KEYS {
        assert_eq!(
            workflow_value(parsed, key).and_then(toml::Value::as_array),
            Some(&Vec::new()),
            "workflow.{key} must be scaffolded as an empty array"
        );
    }
}

fn no_active_role_section(contents: &str) -> bool {
    contents
        .lines()
        .all(|line| !line.trim_start().starts_with("[agent."))
}
