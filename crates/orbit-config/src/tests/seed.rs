//! `orbit init` seeding at the config boundary: the lane crews, the enabled
//! flags and the complexity pools a detected-family set produces [ORB-14671].

use crate::{ConfigSeed, seed_default_config};
use toml::Table;

fn seeded(families: &[&str]) -> Table {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    let seed = ConfigSeed::from_families(families);
    assert!(seed_default_config(&path, Some(&seed)).expect("seed"));
    std::fs::read_to_string(&path)
        .expect("read config")
        .parse()
        .expect("seeded config is valid TOML")
}

fn workflow_pool(config: &Table, key: &str) -> Vec<String> {
    config["workflow"][key]
        .as_array()
        .unwrap_or_else(|| panic!("workflow.{key} is a list"))
        .iter()
        .map(|entry| entry.as_str().expect("pool entry is a string").to_string())
        .collect()
}

fn pools(config: &Table) -> [Vec<String>; 4] {
    [
        "low_complexity_crews",
        "medium_complexity_crews",
        "hard_complexity_crews",
        "xhard_complexity_crews",
    ]
    .map(|key| workflow_pool(config, key))
}

fn crew_enabled(config: &Table, name: &str) -> bool {
    config["crews"][name]["enabled"]
        .as_bool()
        .unwrap_or_else(|| panic!("crews.{name}.enabled is a bool"))
}

fn lane(config: &Table, key: &str) -> Option<String> {
    config["workflow"]
        .get(key)
        .map(|value| value.as_str().expect("lane is a string").to_string())
}

fn expect_pools(config: &Table, expected: [&[&str]; 4], case: &str) {
    let expected = expected.map(|pool| pool.iter().map(ToString::to_string).collect::<Vec<_>>());
    assert_eq!(
        pools(config),
        expected,
        "{case}: pools follow the seed table in docs/CONFIG.md (\"What `orbit init` seeds\")"
    );
}

#[test]
fn claude_only_seeds_haiku_system_crew_and_claude_pools() {
    let config = seeded(&["claude"]);
    assert_eq!(lane(&config, "system_crew").as_deref(), Some("haiku"));
    assert_eq!(lane(&config, "default_crew").as_deref(), Some("opus"));
    expect_pools(
        &config,
        [&["haiku"], &["sonnet"], &["opus"], &["opus"]],
        "claude only",
    );
}

#[test]
fn codex_only_seeds_luna_system_crew_and_codex_pools() {
    let config = seeded(&["codex"]);
    assert_eq!(lane(&config, "system_crew").as_deref(), Some("luna"));
    assert_eq!(lane(&config, "default_crew").as_deref(), Some("astra"));
    expect_pools(
        &config,
        [&["luna"], &["sol"], &["sol"], &["astra"]],
        "codex only",
    );
}

#[test]
fn codex_and_claude_keep_luna_system_crew_and_mixed_pools() {
    let config = seeded(&["codex", "claude"]);
    assert_eq!(lane(&config, "system_crew").as_deref(), Some("luna"));
    assert_eq!(lane(&config, "default_crew").as_deref(), Some("opus"));
    expect_pools(
        &config,
        [
            &["haiku", "luna"],
            &["sol", "sonnet"],
            &["opus"],
            &["opus", "astra"],
        ],
        "codex + claude",
    );
}

#[test]
fn haiku_is_seeded_enabled_exactly_when_claude_is_detected() {
    assert!(crew_enabled(&seeded(&["claude"]), "haiku"));
    assert!(!crew_enabled(&seeded(&["codex"]), "haiku"));
    assert!(!crew_enabled(&seeded(&[]), "haiku"));
    assert_eq!(
        seeded(&["claude"])["crews"]["haiku"]["provider"].as_str(),
        Some("claude")
    );
}

#[test]
fn grok_joins_medium_and_google_cli_joins_low_on_every_base_row() {
    let cases: [(&[&str], [&[&str]; 4]); 4] = [
        (&["claude"], [&["haiku"], &["sonnet"], &["opus"], &["opus"]]),
        (&["codex"], [&["luna"], &["sol"], &["sol"], &["astra"]]),
        (
            &["codex", "claude"],
            [
                &["haiku", "luna"],
                &["sol", "sonnet"],
                &["opus"],
                &["opus", "astra"],
            ],
        ),
        (&[], [&[], &[], &[], &[]]),
    ];
    for (base, expected) in cases {
        let with = |extra: &[&str]| seeded(&[base, extra].concat());
        let grow = |pool: usize, entry: &'static str| {
            let mut grown = expected.map(|pool| pool.to_vec());
            grown[pool].push(entry);
            grown
        };
        let case = format!("{base:?}");

        let config = with(&["grok"]);
        let want = grow(1, "grok");
        expect_pools(&config, want.each_ref().map(Vec::as_slice), &case);

        // `agy` is preferred over the legacy Gemini CLI, alone or together.
        for (extra, entry) in [
            (&["antigravity"][..], "antigravity"),
            (&["gemini"][..], "gemini"),
            (&["gemini", "antigravity"][..], "antigravity"),
        ] {
            let config = with(extra);
            let want = grow(0, entry);
            expect_pools(&config, want.each_ref().map(Vec::as_slice), &case);
        }

        let config = with(&["grok", "antigravity"]);
        let mut want = grow(1, "grok");
        want[0].push("antigravity");
        expect_pools(&config, want.each_ref().map(Vec::as_slice), &case);
    }
}

#[test]
fn families_without_a_pool_rule_leave_pools_empty() {
    for family in ["copilot", "cursor", "pi", "opencode"] {
        let config = seeded(&[family]);
        expect_pools(&config, [&[], &[], &[], &[]], family);
        assert_eq!(lane(&config, "default_crew").as_deref(), Some(family));
    }
}

#[test]
fn host_with_no_cli_seeds_all_crews_disabled_and_no_lane_keys() {
    let config = seeded(&[]);
    expect_pools(&config, [&[], &[], &[], &[]], "no CLI");
    assert_eq!(lane(&config, "default_crew"), None);
    assert_eq!(lane(&config, "system_crew"), None);
    for (name, crew) in config["crews"].as_table().expect("crews table") {
        assert_eq!(crew["enabled"].as_bool(), Some(false), "crew `{name}`");
    }
}

#[test]
fn every_seeded_pool_entry_names_an_enabled_crew() {
    let families = [
        "claude",
        "codex",
        "antigravity",
        "gemini",
        "grok",
        "copilot",
        "cursor",
        "pi",
        "opencode",
    ];
    for mask in 0u32..(1 << families.len()) {
        let detected = families
            .iter()
            .enumerate()
            .filter(|(bit, _)| mask & (1 << bit) != 0)
            .map(|(_, family)| *family)
            .collect::<Vec<_>>();
        let config = seeded(&detected);
        for pool in pools(&config) {
            for name in pool {
                assert!(
                    crew_enabled(&config, &name),
                    "families {detected:?}: pool entry `{name}` must be an enabled seeded crew"
                );
            }
        }
    }
}

#[test]
fn claude_only_interactive_choice_is_haiku_and_codex_hosts_also_offer_it() {
    assert_eq!(
        ConfigSeed::from_families(["claude"]).system_crew_options(),
        ["haiku"]
    );
    assert_eq!(
        ConfigSeed::from_families(["codex", "claude"]).system_crew_options(),
        ["luna", "haiku"]
    );
}

#[test]
fn rendering_refuses_a_pool_entry_the_seed_does_not_write_enabled() {
    let seed = ConfigSeed::from_families(["claude"]);
    let mut crews = seed.seeded_crews();
    crews.get_mut("haiku").expect("haiku is seeded").enabled = false;
    let error = crate::seed::render_workflow_crew_keys(&seed, &crews)
        .expect_err("haiku sits in the low pool and the system lane");
    assert!(
        error.to_string().contains("`haiku`"),
        "error names the offending crew: {error}"
    );

    crews.remove("sonnet");
    crews.get_mut("haiku").expect("haiku is seeded").enabled = true;
    let seed = seed.with_system_crew("haiku");
    let error = crate::seed::render_workflow_crew_keys(&seed, &crews)
        .expect_err("sonnet sits in the medium pool");
    assert!(
        error
            .to_string()
            .contains("workflow.medium_complexity_crews"),
        "error names the pool key: {error}"
    );
}

#[test]
fn seeded_config_loads_and_a_config_without_crews_resolves_haiku() {
    let global = tempfile::tempdir().expect("global tempdir");
    let workspace = tempfile::tempdir().expect("workspace tempdir");

    // Built-in registry: no `[crews]` table at all.
    super::write_config(workspace.path(), "[workflow]\n");
    let config = crate::ResolvedConfig::load(&super::roots(global.path(), workspace.path()))
        .expect("config without crews loads");
    let haiku = &config.crews["haiku"];
    assert_eq!(
        (
            haiku.assignment.provider.as_str(),
            haiku.assignment.model.as_str(),
            haiku.enabled
        ),
        (
            "claude",
            orbit_common::model_defaults::CLAUDE_HAIKU_MODEL,
            true
        )
    );
    let built_in_names = config.crews.keys().map(String::as_str).collect::<Vec<_>>();
    assert_eq!(
        built_in_names,
        [
            "antigravity",
            "astra",
            "copilot",
            "cursor",
            "fable",
            "gemini",
            "grok",
            "haiku",
            "luna",
            "opencode",
            "opus",
            "pi",
            "sol",
            "sonnet",
            "system",
        ],
        "ORB-14672 guards removal of terra from the no-[crews] built-in registry"
    );

    // A seeded file, pools included, passes the same load-time validation.
    let seeded = tempfile::tempdir().expect("seeded tempdir");
    let path = seeded.path().join("config.toml");
    seed_default_config(
        &path,
        Some(&ConfigSeed::from_families(["codex", "claude", "grok"])),
    )
    .expect("seed");
    let seeded_body = std::fs::read_to_string(&path).expect("read");
    let seeded_config: Table = seeded_body.parse().expect("seeded config is valid TOML");
    assert!(
        seeded_config["crews"].get("terra").is_none(),
        "ORB-14672 keeps terra out of fresh init crew tables"
    );
    super::write_config(workspace.path(), &seeded_body);
    crate::ResolvedConfig::load(&super::roots(global.path(), workspace.path()))
        .expect("seeded config loads with its pools");
}
