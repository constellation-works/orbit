//! The two security invariants of a build the install boundary cannot reach
//! without a working build sandbox: what environment a phase receives, and
//! what a hostile build directory can make Orbit install.

use std::path::{Path, PathBuf};

use orbit_types::plugin::{PluginBuildOutput, PluginBuildSpec};

use super::super::build::{
    PLUGIN_BUILD_DENIED_ENV, PLUGIN_BUILD_DENIED_ENV_PREFIXES, PLUGIN_BUILD_TOOLCHAIN_LOCATORS,
    PluginBuildHostEnv, PluginBuildPhase, install_plugin_build_outputs, is_denied_build_env,
    plan_plugin_build, plugin_artifact_digest, plugin_build_environment,
};
use super::super::source::ResolvedCommit;

fn spec(outputs: &[(&str, &str)]) -> PluginBuildSpec {
    PluginBuildSpec {
        programs: vec!["sh".to_string()],
        fetch: None,
        command: vec!["sh".to_string(), "-c".to_string(), "true".to_string()],
        outputs: outputs
            .iter()
            .map(|(from, to)| PluginBuildOutput {
                from: (*from).to_string(),
                to: (*to).to_string(),
            })
            .collect(),
        timeout_ms: None,
    }
}

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

#[cfg(unix)]
fn build_dir_with(files: &[(&str, &[u8], u32)]) -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().expect("build dir");
    for (path, bytes, mode) in files {
        let path = dir.path().join(path);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
        std::fs::write(&path, bytes).expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(*mode)).expect("chmod");
    }
    dir
}

/// §3.4: only a declared output that is a physical, singly linked regular
/// file inside the build directory crosses back, it never replaces a file
/// the reviewed tree ships, and its installed mode keeps only the
/// owner-execute bit.
#[cfg(unix)]
#[test]
fn only_physical_declared_outputs_cross_back_into_the_install() {
    use std::os::unix::fs::PermissionsExt;

    let build = build_dir_with(&[
        ("target/backend", b"#!/bin/sh\n", 0o4775),
        ("target/data.json", b"{}", 0o600),
        ("plugin.yaml", b"hostile", 0o644),
    ]);
    let secret = tempfile::tempdir().expect("outside");
    std::fs::write(secret.path().join("id_ed25519"), "key").expect("secret");
    std::os::unix::fs::symlink(
        secret.path().join("id_ed25519"),
        build.path().join("linked"),
    )
    .expect("link");
    std::os::unix::fs::symlink(secret.path(), build.path().join("linked_dir")).expect("link dir");
    std::fs::hard_link(build.path().join("plugin.yaml"), build.path().join("hard"))
        .expect("hard link");

    let refused = [
        ("linked", "bin/x", "symbolic link"),
        ("linked_dir/id_ed25519", "bin/x", "symbolic link"),
        ("hard", "bin/x", "hard link"),
        ("missing", "bin/x", "was not produced"),
        ("target", "bin/x", "not a regular file"),
        ("target/backend", "shipped.txt", "would replace"),
    ];
    for (from, to, reason) in refused {
        let staging = tempfile::tempdir().expect("staging");
        std::fs::write(staging.path().join("shipped.txt"), "reviewed").expect("pristine");
        let error = install_plugin_build_outputs(
            build.path(),
            &spec(&[(from, to)]).outputs,
            staging.path(),
        )
        .expect_err(&format!("{from} -> {to} must be refused"));
        assert!(
            error.to_string().contains(reason),
            "{from} -> {to}: expected '{reason}' in: {error}"
        );
        assert!(
            !staging.path().join(to).exists() || to == "shipped.txt",
            "nothing is installed from a refused output"
        );
        assert_eq!(
            std::fs::read_to_string(staging.path().join("shipped.txt")).expect("pristine"),
            "reviewed"
        );
    }

    let outputs = [
        ("target/data.json", "share/data.json"),
        ("target/backend", "bin/backend"),
    ];
    let staging = tempfile::tempdir().expect("staging");
    let records =
        install_plugin_build_outputs(build.path(), &spec(&outputs).outputs, staging.path())
            .expect("install outputs");
    let mode = |path: &str| {
        std::fs::metadata(staging.path().join(path))
            .expect("installed")
            .permissions()
            .mode()
            & 0o7777
    };
    assert_eq!(
        mode("bin/backend"),
        0o755,
        "setuid is cleared, owner-execute kept"
    );
    assert_eq!(mode("share/data.json"), 0o644);
    assert_eq!(
        records.iter().map(|record| record.mode).collect::<Vec<_>>(),
        vec![0o644, 0o755]
    );

    // The digest is a function of the installed bytes and modes only.
    let reversed: Vec<(&str, &str)> = outputs.iter().rev().copied().collect();
    let again = tempfile::tempdir().expect("staging");
    let reordered =
        install_plugin_build_outputs(build.path(), &spec(&reversed).outputs, again.path())
            .expect("install outputs");
    assert_eq!(
        plugin_artifact_digest(&records),
        plugin_artifact_digest(&reordered)
    );
}
