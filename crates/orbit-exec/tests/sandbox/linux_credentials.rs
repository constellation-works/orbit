#![cfg(target_os = "linux")]
use super::linux_sandbox::profile;
use orbit_common::OrbitError;
use orbit_exec::compile_linux_bwrap_argv;
use std::{
    fs,
    path::{Path, PathBuf},
};

struct HomeFixture {
    _temp: tempfile::TempDir,
    home: PathBuf,
}

/// A fake HOME holding `.ssh`, `.aws`, `.config/gh` directories and a cargo
/// `credentials.toml` file, but no `.cargo/credentials` and no macOS trees.
fn home_fixture() -> HomeFixture {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().canonicalize().expect("canonical root");
    let home = root.join("home");
    for dir in [".ssh", ".aws", ".config/gh", ".cargo"] {
        fs::create_dir_all(home.join(dir)).expect("create credential dir");
    }
    fs::write(home.join(".ssh/id_ed25519"), b"secret").expect("key");
    fs::write(home.join(".cargo/credentials.toml"), b"token").expect("token");
    HomeFixture { _temp: temp, home }
}

fn has_pair(args: &[String], option: &str, path: &Path) -> bool {
    args.windows(2)
        .any(|pair| pair[0] == option && pair[1] == path.display().to_string())
}

#[test]
fn a_symlinked_credential_dir_masks_its_real_location() {
    if super::audit_support::run_isolated(
        "linux_credentials::a_symlinked_credential_dir_masks_its_real_location",
    ) {
        return;
    }
    let fixture = home_fixture();
    let home = &fixture.home;
    let _environment =
        orbit_common::test_env::scoped([("HOME", home.to_str()), ("CARGO_HOME", None)]);
    let dotfiles = home.join("dotfiles-ssh");
    fs::create_dir_all(&dotfiles).expect("dotfiles");
    fs::remove_dir_all(home.join(".ssh")).expect("remove .ssh");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&dotfiles, home.join(".ssh")).expect("symlink");
    let plan =
        compile_linux_bwrap_argv(&profile(vec![]), "/bin/true", &[], None, false).expect("masks");
    let out = plan.args;

    #[cfg(unix)]
    assert!(
        has_pair(&out, "--tmpfs", &dotfiles),
        "the mount must land on the symlink target, where the bytes live: {out:?}"
    );
}

#[test]
fn a_grant_inside_a_credential_dir_refuses_the_plan_instead_of_hiding_it() {
    if super::audit_support::run_isolated(
        "linux_credentials::a_grant_inside_a_credential_dir_refuses_the_plan_instead_of_hiding_it",
    ) {
        return;
    }
    let fixture = home_fixture();
    let home = &fixture.home;
    let _environment =
        orbit_common::test_env::scoped([("HOME", home.to_str()), ("CARGO_HOME", None)]);
    let granted = home.join(".config/gh/workdir");
    fs::create_dir_all(&granted).expect("granted dir");
    let error = compile_linux_bwrap_argv(
        &profile(vec![granted.display().to_string()]),
        "/bin/true",
        &[],
        None,
        false,
    )
    .expect_err("masking would hide a path the plan grants");

    assert!(
        matches!(&error, OrbitError::PolicyDenied(message) if message.contains("workdir")),
        "the refusal names the granted path: {error}"
    );
}

/// The shared deny list masks the default Linux browser profile roots and
/// leaves a sibling file outside them unmasked. The fixture home is a private
/// temp directory, so this does not depend on a user namespace.
#[test]
fn linux_browser_profile_roots_are_masked_beside_a_readable_file() {
    if super::audit_support::run_isolated(
        "linux_credentials::linux_browser_profile_roots_are_masked_beside_a_readable_file",
    ) {
        return;
    }
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().canonicalize().expect("canonical root");
    let home = root.join("home");
    let readable = root.join("workspace/readable.txt");
    fs::create_dir_all(readable.parent().expect("workspace parent")).expect("workspace");
    fs::write(&readable, b"WORKSPACE-READABLE").expect("workspace file");
    for relative in [
        ".config/google-chrome/Default/Cookies",
        ".config/chromium/Default/Cookies",
        ".config/BraveSoftware/Brave-Browser/Default/Cookies",
        ".mozilla/firefox/review.default/cookies.sqlite",
    ] {
        let path = home.join(relative);
        fs::create_dir_all(path.parent().expect("profile parent")).expect("browser profile");
        fs::write(&path, b"browser-marker").expect("browser marker");
    }
    let _environment =
        orbit_common::test_env::scoped([("HOME", home.to_str()), ("CARGO_HOME", None)]);

    let denies = orbit_exec::default_credential_read_denies();
    let plan = compile_linux_bwrap_argv(
        &profile(vec![]),
        "/bin/cat",
        &[readable.display().to_string()],
        None,
        false,
    )
    .expect("browser roots must compile into masks");
    for browser in [
        ".config/google-chrome",
        ".config/chromium",
        ".config/BraveSoftware/Brave-Browser",
        ".mozilla/firefox",
    ] {
        let masked = home.join(browser);
        assert!(
            denies.iter().any(|deny| deny == &masked),
            "the shared deny list must name {masked:?}: {denies:?}"
        );
        assert!(
            has_pair(&plan.args, "--tmpfs", &masked),
            "the compiled plan must mask {masked:?}: {:?}",
            plan.args
        );
    }
    assert!(
        !has_pair(&plan.args, "--tmpfs", readable.parent().expect("workspace")),
        "a workspace directory beside the browser roots must stay unmasked: {:?}",
        plan.args
    );
}
