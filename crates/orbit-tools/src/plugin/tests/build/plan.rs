use super::super::super::build::{PluginBuildHostEnv, plan_plugin_build};
use super::super::super::source::ResolvedCommit;
use super::spec;

/// A repository-controlled argv must stay one visible plan line even when it
/// contains terminal controls; the same escaping is used for output paths.
#[test]
fn a_build_plan_escapes_terminal_controls_in_repository_values() {
    let checkout = tempfile::tempdir().expect("checkout");
    let commit = ResolvedCommit {
        id: "a".repeat(40),
        committed_at: 1_700_000_000,
        checkout: checkout.path().to_path_buf(),
    };
    let mut build = spec(&[("out\n\u{1b}[2J", "bin/out")]);
    build.programs = vec!["/bin/sh".to_string()];
    build.command = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        "true\n  source: forged\u{1b}[2J".to_string(),
    ];
    let plan = plan_plugin_build(
        &build,
        "git+https://example.test/demo.git#aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        &commit,
        &PluginBuildHostEnv::default(),
        &[],
    )
    .expect("plan");

    let rendered = plan.render();
    assert!(
        rendered.contains(r#""true\n  source: forged\u{1b}[2J""#),
        "the argv is rendered with control characters escaped: {rendered:?}"
    );
    assert!(
        rendered.contains(r#""out\n\u{1b}[2J" -> "bin/out""#),
        "output paths are rendered with control characters escaped: {rendered:?}"
    );
    assert!(
        !rendered.contains('\u{1b}'),
        "a repository value must not emit terminal control bytes"
    );
}

/// A rustup toolchains link must not make a lexical installation path bind
/// a forbidden workspace or credential tree into the build sandbox.
#[cfg(unix)]
#[test]
fn a_linked_toolchain_root_is_refused_before_a_build() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().expect("operator home");
    let bin = home.path().join(".cargo/bin");
    let rustup = home.path().join(".rustup");
    std::fs::create_dir_all(&bin).expect("cargo bin");
    std::fs::create_dir(&rustup).expect("rustup home");
    let cargo = bin.join("cargo");
    std::fs::write(&cargo, "#!/bin/sh\nexit 0\n").expect("proxy");
    std::fs::set_permissions(&cargo, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let forbidden = tempfile::tempdir().expect("workspace");
    std::os::unix::fs::symlink(forbidden.path(), rustup.join("toolchains")).expect("link");
    let host = PluginBuildHostEnv {
        path: Some(bin.into_os_string()),
        home: Some(home.path().to_path_buf()),
        locators: Vec::new(),
    };
    let mut build = spec(&[("out", "bin/out")]);
    build.programs = vec!["cargo".to_string()];
    build.command = vec!["cargo".to_string(), "build".to_string()];
    let commit = ResolvedCommit {
        id: "a".repeat(40),
        committed_at: 1_700_000_000,
        checkout: forbidden.path().to_path_buf(),
    };
    let error = plan_plugin_build(
        &build,
        "git+https://example.test/demo.git#aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        &commit,
        &host,
        &[forbidden.path().to_path_buf()],
    )
    .expect_err("a linked toolchain must not expose a forbidden tree");
    assert!(matches!(error, orbit_common::OrbitError::PolicyDenied(_)));
}
