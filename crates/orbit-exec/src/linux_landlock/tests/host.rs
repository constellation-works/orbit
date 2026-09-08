//! What the host grant table does and does not admit.

use std::fs;
use std::path::{Path, PathBuf};

use super::super::host::{HOST_READ_ENV_VARS, host_read_grants};
use crate::linux_landlock::grants_read;

fn environment(pairs: &[(&str, &Path)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(name, path)| (name.to_string(), path.display().to_string()))
        .collect()
}

#[test]
fn the_loader_and_system_binaries_are_granted_so_a_program_can_start() {
    let grants = host_read_grants(&[]);

    assert!(grants_read(&grants, Path::new("/bin/sh")), "{grants:?}");
    assert!(grants_read(&grants, Path::new("/dev/null")), "{grants:?}");
}

/// `/etc` holds `shadow` and every application's secrets. Individual
/// resolver files are granted; the directory never is.
#[test]
fn resolver_files_are_granted_without_granting_etc() {
    let grants = host_read_grants(&[]);

    assert!(grants_read(&grants, Path::new("/etc/hosts")), "{grants:?}");
    assert!(
        !grants_read(&grants, Path::new("/etc/shadow")),
        "{grants:?}"
    );
}

/// A tree-wide `/proc` grant would expose any same-user process's `environ`,
/// including the credentials of the process that launched the child.
#[test]
fn proc_is_not_granted_as_a_tree() {
    let grants = host_read_grants(&[]);

    assert!(
        !grants_read(
            &grants,
            &PathBuf::from(format!("/proc/{}/environ", std::process::id()))
        ),
        "{grants:?}"
    );
}

#[test]
fn a_declared_tool_state_directory_is_granted_but_its_publish_token_is_not() {
    let home = tempfile::tempdir().expect("home");
    let cargo_home = home.path().join("cargo");
    fs::create_dir_all(cargo_home.join("registry")).expect("mkdir registry");
    fs::write(cargo_home.join("config.toml"), "config").expect("write config");
    fs::write(cargo_home.join("credentials.toml"), "token").expect("write credentials");

    let grants = host_read_grants(&environment(&[("CARGO_HOME", &cargo_home)]));

    assert!(
        grants_read(&grants, &cargo_home.join("registry/index")),
        "{grants:?}"
    );
    assert!(
        grants_read(&grants, &cargo_home.join("config.toml")),
        "{grants:?}"
    );
    assert!(
        !grants_read(&grants, &cargo_home.join("credentials.toml")),
        "{grants:?}"
    );
}

/// The table's whole purpose is to be narrower than `$HOME`. A variable that
/// names the home directory itself, or an ancestor of it, is refused rather
/// than silently widening every grant.
#[test]
fn a_tool_state_variable_cannot_widen_the_grant_to_the_home_directory() {
    let home = tempfile::tempdir().expect("home");
    let secret = home.path().join(".ssh/id_ed25519");
    fs::create_dir_all(home.path().join(".ssh")).expect("mkdir .ssh");
    fs::write(&secret, "key").expect("write key");

    for named_home in [home.path(), Path::new("/")] {
        let grants = host_read_grants(&environment(&[
            ("HOME", home.path()),
            ("CARGO_HOME", named_home),
        ]));
        assert!(!grants_read(&grants, &secret), "{named_home:?}: {grants:?}");
    }
}

/// An unlisted credential store stays unreadable even when `$HOME` is known.
#[test]
fn an_unlisted_credential_store_is_never_granted() {
    let home = tempfile::tempdir().expect("home");
    for relative in [".ssh/id_ed25519", ".aws/credentials", ".netrc"] {
        let path = home.path().join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("mkdir");
        }
        fs::write(&path, "secret").expect("write secret");
    }

    let grants = host_read_grants(&environment(&[("HOME", home.path())]));

    for relative in [".ssh/id_ed25519", ".aws/credentials", ".netrc"] {
        assert!(
            !grants_read(&grants, &home.path().join(relative)),
            "{relative} was granted: {grants:?}"
        );
    }
}

/// A shipped allowlist names programs, not locations, so `PATH` is what turns
/// `rg` into a grant.
#[test]
fn path_directories_are_granted_so_an_allowlisted_program_can_be_executed() {
    let tools = tempfile::tempdir().expect("tools");
    let program = tools.path().join("rg");
    fs::write(&program, "#!/bin/sh\n").expect("write program");

    let grants = host_read_grants(&environment(&[("PATH", tools.path())]));

    assert!(grants_read(&grants, &program), "{grants:?}");
}

/// The documented list is what an operator reads to understand how a child
/// environment turns into host access, so it must not drift from the code.
#[test]
fn every_documented_variable_actually_widens_the_grants() {
    let home = tempfile::tempdir().expect("home");
    let baseline = host_read_grants(&environment(&[("HOME", home.path())])).len();

    for variable in HOST_READ_ENV_VARS {
        // `XDG_CONFIG_HOME` names a parent of per-tool directories rather than
        // a tool's own state, so the grant lands beneath it.
        let named = home.path().join(variable.to_lowercase());
        fs::create_dir_all(named.join("git")).expect("mkdir named directory");
        let grants = host_read_grants(&environment(&[("HOME", home.path()), (variable, &named)]));

        assert!(
            grants.iter().any(|grant| grant.path.starts_with(&named)),
            "`{variable}` granted nothing at or beneath the directory it names"
        );
        assert!(
            grants.len() > baseline,
            "`{variable}` added no grant at all"
        );
    }
}
