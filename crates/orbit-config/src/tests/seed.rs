//! `orbit init` seeding at the config boundary: the default crew, the enabled
//! flags and valid pool references across detected-family combinations [ORB-14671].
//! No system lane is seeded: system work draws from the pools [ORB-15663].

use crate::ConfigSeed;
use toml::Table;

const POOL_KEYS: [&str; 4] = [
    "low_complexity_crews",
    "medium_complexity_crews",
    "hard_complexity_crews",
    "xhard_complexity_crews",
];

fn workflow_pool(config: &Table, key: &str) -> Vec<String> {
    config["workflow"][key]
        .as_array()
        .unwrap_or_else(|| panic!("workflow.{key} is a list"))
        .iter()
        .map(|entry| entry.as_str().expect("pool entry is a string").to_string())
        .collect()
}

fn pools(config: &Table) -> [Vec<String>; 4] {
    POOL_KEYS.map(|key| workflow_pool(config, key))
}

fn lane(config: &Table, key: &str) -> Option<String> {
    config["workflow"]
        .get(key)
        .map(|value| value.as_str().expect("lane is a string").to_string())
}

#[test]
fn every_family_combination_seeds_valid_defaults_pools_and_enabled_flags() {
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
        let seed = ConfigSeed::from_families(&detected);
        let crews = seed.seeded_crews();
        let workflow = crate::seed::render_workflow_crew_keys(&seed, &crews)
            .expect("family combination renders valid workflow keys");
        let config: Table = format!("[workflow]\n{workflow}")
            .parse()
            .expect("rendered workflow is valid TOML");
        assert_eq!(
            lane(&config, "system_crew"),
            None,
            "ORB-15663: fresh seeds never write the retired system lane"
        );
        if detected.is_empty() {
            assert_eq!(lane(&config, "default_crew"), None);
        } else {
            let default = lane(&config, "default_crew").expect("detected families have a default");
            assert!(!default.trim().is_empty());
            assert!(
                crews.get(&default).is_some_and(|crew| crew.enabled),
                "families {detected:?}: default `{default}` names an enabled defined crew"
            );
        }
        for (name, crew) in &crews {
            assert_eq!(
                crew.enabled,
                detected.contains(&crew.assignment.provider.as_str()),
                "families {detected:?}: crew `{name}` follows its provider's availability"
            );
            assert!(
                !crew.assignment.model.trim().is_empty(),
                "crew `{name}` has a model"
            );
        }
        for (key, pool) in POOL_KEYS.into_iter().zip(pools(&config)) {
            let canonical = crate::canonical_crew_pool(&pool, &crews, key)
                .expect("rendered pool parses and references defined crews");
            for entry in canonical.entries {
                assert!(
                    crews[&entry.name].enabled,
                    "families {detected:?}: pool entry `{}` must be enabled",
                    entry.name
                );
            }
        }
    }
}

#[test]
fn rendering_refuses_a_pool_entry_the_seed_does_not_write_enabled() {
    let seed = ConfigSeed::from_families(["claude", "codex", "grok"]);
    let crews = seed.seeded_crews();
    let pools = seed.complexity_pools();
    let (key, victim) = POOL_KEYS
        .into_iter()
        .zip(pools)
        .find_map(|(key, pool)| pool.first().copied().map(|name| (key, name)))
        .expect("detected families seed a pool entry to fault");
    let default = crews
        .iter()
        .find(|(name, crew)| crew.enabled && name.as_str() != victim)
        .map(|(name, _)| name.clone())
        .expect("another enabled crew can be the default");
    let seed = seed.with_default_crew(default);

    for missing in [false, true] {
        let mut crews = crews.clone();
        if missing {
            crews.remove(victim);
        } else {
            crews
                .get_mut(victim)
                .expect("pool member is defined")
                .enabled = false;
        }
        let error = crate::seed::render_workflow_crew_keys(&seed, &crews)
            .expect_err("missing or disabled pool member is refused");
        assert!(
            error.to_string().contains(&format!("workflow.{key}")),
            "error names the offending pool: {error}"
        );
        assert!(
            error.to_string().contains(&format!("`{victim}`")),
            "error names the offending crew: {error}"
        );
    }
}
