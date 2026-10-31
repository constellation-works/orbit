use super::default_crew_write_config;
use orbit_config::ConfigRoots;

use orbit_config::{ConfigSeed, seed_default_config};

#[test]
fn seeded_and_builtin_configs_load_valid_crew_registries() {
    let global = tempfile::tempdir().expect("global tempdir");
    let workspace = tempfile::tempdir().expect("workspace tempdir");

    // Built-in registry: no `[crews]` table at all.
    default_crew_write_config(workspace.path(), "[workflow]\n");
    let config =
        orbit_config::ResolvedConfig::load(&ConfigRoots::new(global.path(), workspace.path()))
            .expect("config without crews loads");
    assert!(!config.crews.is_empty());
    for (name, crew) in &config.crews {
        assert!(!name.trim().is_empty());
        assert!(!crew.assignment.model.trim().is_empty());
        assert!(!crew.assignment.provider.trim().is_empty());
    }
    assert!(
        !config.crews.contains_key("system"),
        "ORB-15663 keeps the retired system crew out of the built-in registry"
    );

    // Every built-in crew can be selected through the ordinary config path.
    for (name, crew) in &config.crews {
        default_crew_write_config(
            workspace.path(),
            &format!("[workflow]\ndefault_crew = {name:?}\n"),
        );
        let selected =
            orbit_config::ResolvedConfig::load(&ConfigRoots::new(global.path(), workspace.path()))
                .expect("built-in crew can be selected");
        assert_eq!(selected.default_crew.as_deref(), Some(name.as_str()));
        assert_eq!(selected.crews[name].assignment, crew.assignment);
    }

    // Round-trip provider-dependent registries and pools through the public
    // seeding/loading boundary, without freezing the current policy choices.
    for families in [
        vec![],
        vec!["codex"],
        vec!["codex", "claude", "antigravity", "grok", "copilot", "muse"],
        vec!["claude", "antigravity", "grok", "copilot", "muse"],
    ] {
        let seeded = tempfile::tempdir().expect("seeded tempdir");
        let path = seeded.path().join("config.toml");
        let seed = ConfigSeed::from_families(families);
        seed_default_config(&path, Some(&seed)).expect("seed");
        let seeded_body = std::fs::read_to_string(&path).expect("read");
        default_crew_write_config(workspace.path(), &seeded_body);
        let loaded =
            orbit_config::ResolvedConfig::load(&ConfigRoots::new(global.path(), workspace.path()))
                .expect("seeded config loads with its pools");
        assert_eq!(loaded.crews, seed.seeded_crews());

        // Reinitializing an existing file must preserve its contents even
        // when a different provider set is detected.
        assert!(!seed_default_config(&path, Some(&ConfigSeed::default())).expect("reseed"));
        assert_eq!(std::fs::read_to_string(&path).expect("reread"), seeded_body);
    }
}
