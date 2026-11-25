//! Guarded GitHub reads use the host's configured credential route.
#![allow(missing_docs)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use orbit_common::test_env;
use orbit_engine::run_private_vcs_operation;
use serde_json::json;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

const CHILD_ENV: &str = "ORBIT_TEST_GUARDED_GH_READ";
const TEST_NAME: &str =
    "guarded_gh_reads::guarded_gh_read_uses_the_credential_route_and_distinguishes_refusals";

#[test]
fn guarded_gh_read_uses_the_credential_route_and_distinguishes_refusals() {
    if std::env::var_os(CHILD_ENV).is_some() {
        exercise_guarded_read();
        return;
    }

    let sandbox = TempDir::new_in(test_env::canonical_temp_dir()).unwrap();
    let home = sandbox.path().join("home");
    let bin = sandbox.path().join("bin");
    let runtime = sandbox.path().join("runtime");
    for directory in [&home, &bin, &runtime] {
        fs::create_dir_all(directory).unwrap();
    }
    let mut path = vec![bin];
    path.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));

    let mut child = Command::new(std::env::current_exe().unwrap());
    test_env::clear_inherited_authority(|name| {
        child.env_remove(name);
    });
    for (name, _) in std::env::vars_os() {
        let name = name.to_string_lossy();
        if name.starts_with("ORBIT_") || name.starts_with("GH_") || name.starts_with("GITHUB_") {
            child.env_remove(name.as_ref());
        }
    }
    for name in ["ANTHROPIC_API_KEY", "OPENAI_API_KEY"] {
        child.env_remove(name);
    }
    child
        .args([TEST_NAME, "--exact", "--nocapture", "--test-threads=1"])
        .current_dir(sandbox.path())
        .env(CHILD_ENV, "1")
        .env("PATH", std::env::join_paths(path).unwrap())
        .env("HOME", &home)
        .env("GH_CONFIG_DIR", home.join(".config/gh"))
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_RUNTIME_DIR", &runtime)
        .env(
            "DBUS_SESSION_BUS_ADDRESS",
            format!("unix:path={}", runtime.join("bus").display()),
        )
        .env("ANTHROPIC_API_KEY", "provider-secret")
        .env("ORBIT_OPERATOR", "1");
    let output = test_env::run_child_test(&mut child, TEST_NAME, sandbox.path());
    test_env::assert_child_test_passed(TEST_NAME, output.status, &output.stdout, &output.stderr);
}

fn exercise_guarded_read() {
    let workspace = std::env::current_dir().unwrap();
    let inherited_path = std::env::var_os("PATH").unwrap();
    let bin = std::env::split_paths(&inherited_path).next().unwrap();
    let capture = workspace.join("gh-environment-names");
    write_gh(
        &bin,
        &format!(
            "env | cut -d= -f1 > {}\nprintf '%s\\n' '{{\"number\":42}}'",
            shell_quote(&capture)
        ),
    );
    let input = json!({
        "pr": "42",
        "workspace_path": workspace.to_string_lossy(),
    });
    let result =
        run_private_vcs_operation(&orbit_common::Env::capture(), "pr.status", &input).unwrap();
    assert_eq!(result["pull_request"]["number"], 42);

    let names = fs::read_to_string(&capture).unwrap();
    for expected in [
        "DBUS_SESSION_BUS_ADDRESS",
        "XDG_RUNTIME_DIR",
        "GH_CONFIG_DIR",
    ] {
        assert!(
            names.lines().any(|name| name == expected),
            "{expected} missing from {names}"
        );
    }
    for withheld in ["ANTHROPIC_API_KEY", "ORBIT_OPERATOR"] {
        assert!(
            !names.lines().any(|name| name == withheld),
            "{withheld} leaked to gh"
        );
    }

    write_gh(
        &bin,
        "echo 'To get started with GitHub CLI, please run:  gh auth login' >&2; exit 4",
    );
    let error = run_private_vcs_operation(&orbit_common::Env::capture(), "pr.status", &input)
        .expect_err("an unauthenticated read fails");
    assert!(
        error.to_string().contains("credentials are unavailable"),
        "{error}"
    );

    write_gh(
        &bin,
        "echo 'GraphQL: Could not resolve to a PullRequest (HTTP 404)' >&2; exit 1",
    );
    let error = run_private_vcs_operation(&orbit_common::Env::capture(), "pr.status", &input)
        .expect_err("a rejected read fails");
    assert!(!error.to_string().contains("credentials"), "{error}");
}

fn write_gh(bin: &Path, body: &str) {
    let path = bin.join("gh");
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}
