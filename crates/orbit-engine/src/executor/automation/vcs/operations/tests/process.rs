//! The private VCS operations' `gh` reads under the host's GitHub quota
//! [ORB-25868]: a completion stopped as an untyped failure when GitHub's REST
//! quota ran out (PR #10137), and every reader kept calling after the first
//! refusal.

use orbit_common::OrbitError;
use orbit_exec::{EnvironmentMode, ExecRequest, StdinMode};
use orbit_tools::github_quota::{self, FixtureGh, GithubQuotaPolicy, GithubQuotaStore};
use tempfile::TempDir;

use super::super::process::{execute, execute_with_transient_retry};

const CHECK_RUNS: [&str; 2] = ["api", "repos/o/r/commits/abc/check-runs"];

fn args() -> Vec<String> {
    CHECK_RUNS.iter().map(ToString::to_string).collect()
}

fn throttle_of(error: &OrbitError) -> Option<orbit_common::ProviderReadThrottle> {
    error.private_vcs_failure()?.read_throttle.clone()
}

/// A delivery read of a held resource is refused with the typed throttle and
/// runs nothing.
#[test]
fn held_core_refuses_a_delivery_read_before_gh_runs() {
    let scratch = TempDir::new().unwrap();
    let records = TempDir::new().unwrap();
    let gh = FixtureGh::create(scratch.path(), false).unwrap();
    let request = ExecRequest {
        program: gh.program(),
        args: args(),
        current_dir: None,
        timeout_ms: None,
        stdin_mode: StdinMode::Null,
        environment_mode: EnvironmentMode::ClearAndSet(
            crate::executor::automation::vcs::git::gh_environment(),
        ),
        debug: false,
    };
    let account = github_quota::request_account_key(&request);
    let store = GithubQuotaStore::new(records.path().to_path_buf());
    let policy = GithubQuotaPolicy::default();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let reset = now + 900;
    store.record_throttle(
        &policy,
        &account,
        github_quota::RESOURCE_CORE,
        &orbit_common::ProviderReadThrottle {
            reset_epoch_seconds: Some(reset),
            principal: Some("user ID 42".into()),
            ..Default::default()
        },
        now,
    );

    let error = github_quota::with_scoped_guard(store, policy, || {
        execute_with_transient_retry(&gh.program(), &args(), None, 10_000, "check runs")
    })
    .expect_err("a held read must be refused");
    let throttle = throttle_of(&error).expect("typed throttle");
    assert_eq!(throttle.resource.as_deref(), Some("core"));
    assert_eq!(throttle.reset_epoch_seconds, Some(reset));
    // Only the free rate-limit endpoint may be read; the refused read never runs.
    assert_eq!(gh.calls(), vec!["api rate_limit".to_string()]);
}

/// Criterion 8: GitHub's authenticated rate-limit answer to a private VCS read
/// carries the typed throttle, read once and never retried as transient.
#[test]
fn rate_limited_private_vcs_read_carries_the_typed_throttle() {
    let scratch = TempDir::new().unwrap();
    let gh = FixtureGh::create(scratch.path(), true).unwrap();
    let error = execute_with_transient_retry(&gh.program(), &args(), None, 10_000, "check runs")
        .expect_err("a throttled read fails");
    let throttle = throttle_of(&error).expect("typed throttle");
    assert_eq!(throttle.principal.as_deref(), Some("user ID 42"));
    assert_eq!(gh.calls(), vec![CHECK_RUNS.join(" ")]);
}

fn script_gh(dir: &std::path::Path, body: &str) -> String {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("gh");
    orbit_common::fs::io::atomic_write_text(&path, &format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path.to_string_lossy().into_owned()
}

/// A guarded read reaches the same configured credential route as the
/// governed GitHub reader: the login keyring's session bus and runtime
/// directory arrive, while a provider secret and the Orbit envelope do not
/// [ORB-28025]. An isolated child supplies the parent environment without
/// racing other tests.
#[test]
fn guarded_gh_read_gets_the_credential_route_and_no_provider_secret() {
    const TEST: &str = "executor::automation::vcs::operations::tests::process::guarded_gh_read_gets_the_credential_route_and_no_provider_secret";
    const CHILD: &str = "ORBIT_TEST_GUARDED_GH_ENV";
    if std::env::var_os(CHILD).is_some() {
        let scratch = TempDir::new().unwrap();
        let gh = script_gh(scratch.path(), "env");
        let result = execute(&gh, args(), None, 10_000, "check runs").unwrap();
        let names: Vec<&str> = result
            .stdout
            .lines()
            .filter_map(|line| line.split_once('=').map(|(name, _)| name))
            .collect();
        for expected in [
            "DBUS_SESSION_BUS_ADDRESS",
            "XDG_RUNTIME_DIR",
            "GH_CONFIG_DIR",
        ] {
            assert!(
                names.contains(&expected),
                "{expected} missing from {names:?}"
            );
        }
        for withheld in ["ANTHROPIC_API_KEY", "ORBIT_OPERATOR"] {
            assert!(!names.contains(&withheld), "{withheld} leaked to gh");
        }
        return;
    }
    let output_dir = TempDir::new().unwrap();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap());
    child
        .args(["--exact", TEST, "--nocapture"])
        .env(CHILD, "1")
        .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/run/user/1000/bus")
        .env("XDG_RUNTIME_DIR", "/run/user/1000")
        .env("GH_CONFIG_DIR", "/home/svc/.config/gh")
        .env("ANTHROPIC_API_KEY", "provider-secret")
        .env("ORBIT_OPERATOR", "1");
    let output = orbit_common::test_env::run_child_test(&mut child, TEST, output_dir.path());
    orbit_common::test_env::assert_child_test_passed(
        TEST,
        output.status,
        &output.stdout,
        &output.stderr,
    );
}

/// A missing credential is named as such, apart from an ordinary refusal such
/// as an unknown pull request, so landing reports why it could not read the
/// reviewed head [ORB-28025].
#[test]
fn unavailable_credentials_are_named_apart_from_other_refusals() {
    let scratch = TempDir::new().unwrap();
    let unauthenticated = script_gh(
        scratch.path(),
        "echo 'To get started with GitHub CLI, please run:  gh auth login' >&2; exit 4",
    );
    let error = execute(&unauthenticated, args(), None, 10_000, "pr view")
        .expect_err("an unauthenticated read fails");
    assert!(
        error.to_string().contains("credentials are unavailable"),
        "{error}"
    );
    let rejected = script_gh(
        scratch.path(),
        "echo 'GraphQL: Could not resolve to a PullRequest (HTTP 404)' >&2; exit 1",
    );
    let error =
        execute(&rejected, args(), None, 10_000, "pr view").expect_err("a rejected read fails");
    assert!(!error.to_string().contains("credentials"), "{error}");
}
