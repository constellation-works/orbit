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
