#[test]
fn agent_stall_minutes_defaults_to_thirty_and_zero_disables() {
    let global = tempfile::tempdir().expect("global tempdir");
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    std::fs::write(global.path().join("config.toml"), "").expect("write global config");

    let load = || {
        orbit_config::ResolvedConfig::load(&orbit_config::ConfigRoots::new(
            global.path(),
            workspace.path(),
        ))
    };

    let defaults = load().expect("defaults");
    assert_eq!(defaults.snapshot.execution_agent_stall_minutes, 30);

    std::fs::write(
        workspace.path().join("config.toml"),
        "[execution]\nagent_stall_minutes = 0\n",
    )
    .expect("write disabled config");
    let disabled = load().expect("zero disables");
    assert_eq!(disabled.snapshot.execution_agent_stall_minutes, 0);

    std::fs::write(
        workspace.path().join("config.toml"),
        "[execution]\nagent_stall_minutes = 15\n",
    )
    .expect("write custom config");
    let custom = load().expect("custom");
    assert_eq!(custom.snapshot.execution_agent_stall_minutes, 15);

    std::fs::write(
        workspace.path().join("config.toml"),
        "[execution]\nagent_stall_minutes = 1441\n",
    )
    .expect("write out-of-range config");
    let error = load().expect_err("above a day").to_string();
    assert!(
        error.contains("execution.agent_stall_minutes") && error.contains("1441"),
        "{error}"
    );
}
