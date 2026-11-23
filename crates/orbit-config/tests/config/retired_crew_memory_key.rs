//  Crew memory's retired switch through the public loader: a config that
//  still sets `crew_memory.enabled` loads, warns that the key is deprecated
//  and ignored, and is not reported as unknown.

#[test]
fn crew_memory_enabled_key_loads_with_a_deprecation_warning() {
    let (_config, logs) = load_workspace("[crew_memory]\nenabled = true\n");

    assert!(
        logs.lines().any(|line| line.contains("crew_memory.enabled")
            && line.contains("config key is deprecated")),
        "the retired key warns as deprecated: {logs}"
    );
    assert!(
        !logs.lines().any(|line| line.contains("unknown config key")),
        "the retired key is not reported as unknown: {logs}"
    );
    admit_settable_config_key("crew_memory.enabled").expect_err("the retired key is not settable");
}
