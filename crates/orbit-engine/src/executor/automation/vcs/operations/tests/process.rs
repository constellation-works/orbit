//! The private VCS operations' `gh` reads under the host's GitHub quota
//! [ORB-25868]: a completion stopped as an untyped failure when GitHub's REST
//! quota ran out (PR #10137), and every reader kept calling after the first
//! refusal.

use orbit_common::OrbitError;
use orbit_exec::{EnvironmentMode, ExecRequest, StdinMode};
use orbit_tools::github_quota::{self, FixtureGh, GithubQuotaPolicy, GithubQuotaStore};
use tempfile::TempDir;

use super::super::process::execute_with_transient_retry;

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
