//  Ignored optional crew properties name the config file that set them. A
//  layered load merges the two files, so attribution must come from the layer
//  that supplied the field, not from the merged document's path.

fn ignored_crew_write_config(root: &Path, body: &str) {
    std::fs::write(root.join("config.toml"), body).expect("write config");
}

fn config_file_display(root: &Path) -> String {
    redact_home_dir(&root.join("config.toml").display().to_string())
}

#[test]
fn ignored_crew_effort_names_global_file_when_only_global_sets_it() {
    let global = tempfile::tempdir().expect("global tempdir");
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    ignored_crew_write_config(
        global.path(),
        r#"
[workflow]
default_crew = "sol"

[crews.sol]
model = "m"
provider = "codex"
effort = "bogus"
"#,
    );
    ignored_crew_write_config(workspace.path(), "[workflow]\nauto_ship = true\n");

    let config = ResolvedConfig::load(&ConfigRoots::new(global.path(), workspace.path()))
        .expect("layered config with an ignored effort loads");

    assert_eq!(config.ignored_crew_properties.len(), 1);
    let ignored = config
        .ignored_crew_properties
        .first()
        .expect("invalid global effort is reported as ignored");
    assert_eq!(
        ignored.config,
        config_file_display(global.path()),
        "the global file supplied the ignored effort, so it must be named"
    );
    assert!(
        ignored.remediation().contains(&ignored.config),
        "remediation must point at the file that holds the bad value: {}",
        ignored.remediation(),
    );
}

#[test]
fn ignored_crew_effort_names_workspace_file_when_workspace_sets_it() {
    let global = tempfile::tempdir().expect("global tempdir");
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    ignored_crew_write_config(
        global.path(),
        r#"
[workflow]
default_crew = "sol"

[crews.sol]
model = "m"
provider = "codex"
effort = "high"
"#,
    );
    ignored_crew_write_config(
        workspace.path(),
        r#"
[crews.sol]
effort = "bogus"
"#,
    );

    let config = ResolvedConfig::load(&ConfigRoots::new(global.path(), workspace.path()))
        .expect("layered config with an ignored effort loads");

    assert_eq!(config.ignored_crew_properties.len(), 1);
    let ignored = config
        .ignored_crew_properties
        .first()
        .expect("invalid workspace effort is reported as ignored");
    assert_eq!(
        ignored.config,
        config_file_display(workspace.path()),
        "the workspace file supplied the ignored effort, so it must be named"
    );
}
