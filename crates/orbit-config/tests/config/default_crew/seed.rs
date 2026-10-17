use super::default_crew_write_config;
use orbit_config::ConfigRoots;

use orbit_config::{ConfigSeed, seed_default_config};
use toml::Table;

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
        !config.crews.contains_key("terra"),
        "ORB-14672 keeps the retired terra crew out of the built-in registry"
    );
    assert!(
        !config.crews.contains_key("system"),
        "ORB-15663 keeps the retired system crew out of the built-in registry"
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
    default_crew_write_config(workspace.path(), &seeded_body);
    orbit_config::ResolvedConfig::load(&ConfigRoots::new(global.path(), workspace.path()))
        .expect("seeded config loads with its pools");
}
