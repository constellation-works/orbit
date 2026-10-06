use std::path::{Path, PathBuf};

use super::super::super::build::{
    PLUGIN_BUILD_DENIED_ENV, PLUGIN_BUILD_DENIED_ENV_PREFIXES, PLUGIN_BUILD_TOOLCHAIN_LOCATORS,
    PluginBuildHostEnv, PluginBuildPhase, is_denied_build_env, plan_plugin_build,
    plugin_build_environment,
};
use super::super::super::source::ResolvedCommit;
use super::spec;

/// §3.5: the locator list and the denylist never overlap, and a phase's
/// environment holds no denied name, no operator variable outside the
/// locators the plan decided, and no `ORBIT_*` name but the three build ones
/// — even when the operator's shell carries every one of them.
#[test]
fn a_build_environment_never_carries_a_denied_or_ambient_name() {
    for locator in PLUGIN_BUILD_TOOLCHAIN_LOCATORS {
        assert!(
            !is_denied_build_env(locator),
            "locator {locator} is on the build denylist; the denylist must win"
        );
    }
    let mut operator: Vec<(String, String)> = PLUGIN_BUILD_DENIED_ENV
        .iter()
        .map(|name| ((*name).to_string(), "secret".to_string()))
        .collect();
    operator.extend(
        PLUGIN_BUILD_DENIED_ENV_PREFIXES
            .iter()
            .map(|prefix| (format!("{prefix}SECRET"), "secret".to_string())),
    );
    // A locator outside every consented root is not passed.
    operator.push(("GOROOT".to_string(), "/".to_string()));
    let host = PluginBuildHostEnv {
        path: Some("/usr/bin:/bin".into()),
        home: Some(PathBuf::from("/nonexistent-home")),
        locators: operator,
    };
    let checkout = tempfile::tempdir().expect("checkout");
    let commit = ResolvedCommit {
        id: "a".repeat(40),
        committed_at: 1_700_000_000,
        checkout: checkout.path().to_path_buf(),
    };
    let plan = plan_plugin_build(
        &spec(&[("out", "bin/out")]),
        "git+https://user:token@example.test/demo.git#aaaa",
        &commit,
        &host,
        &[],
    )
    .expect("plan");
    assert!(
        !plan.source.contains("token"),
        "the recorded source drops URL credentials: {}",
        plan.source
    );
    for phase in [PluginBuildPhase::Fetch, PluginBuildPhase::Build] {
        let env = plugin_build_environment(&plan, Path::new("/b"), phase);
        let names: Vec<&str> = env.iter().map(|(name, _)| name.as_str()).collect();
        for name in &names {
            assert!(!is_denied_build_env(name), "{name} reached a build");
            assert!(
                !name.starts_with("ORBIT_")
                    || ["ORBIT_BUILD_DIR", "ORBIT_BUILD_SRC", "ORBIT_BUILD_PHASE"].contains(name),
                "{name} reached a build"
            );
        }
        assert!(
            !names.contains(&"GOROOT"),
            "an unconsented locator reached a build"
        );
        let get = |key: &str| {
            env.iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.as_str())
        };
        assert_eq!(get("HOME"), Some("/b/home"));
        assert_eq!(get("CARGO_HOME"), Some("/b/home/.cargo"));
        assert_eq!(get("SOURCE_DATE_EPOCH"), Some("1700000000"));
        assert_eq!(get("ORBIT_BUILD_PHASE"), Some(phase.name()));
    }
}
