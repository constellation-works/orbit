//! The sweeps' `gh` reads under the host's GitHub quota [ORB-25868]: a
//! background read at the background reserve is refused with the typed
//! throttle the sweep already waits out, and runs nothing.

use std::collections::BTreeMap;

use orbit_exec::{EnvironmentMode, ExecRequest, StdinMode};
use orbit_tools::github_quota::{
    self, FixtureGh, GithubQuotaPolicy, GithubQuotaStore, ObservedQuota, RateLimitAnswer,
};
use tempfile::TempDir;

use super::super::github_host::run_gh;

#[test]
fn background_read_at_the_background_reserve_runs_nothing() {
    let scratch = TempDir::new().unwrap();
    let records = TempDir::new().unwrap();
    let gh = FixtureGh::create(scratch.path(), false).unwrap();
    let request = ExecRequest {
        program: gh.program(),
        args: vec!["run".into(), "list".into()],
        current_dir: None,
        timeout_ms: Some(10_000),
        stdin_mode: StdinMode::Null,
        environment_mode: EnvironmentMode::ClearAndSet(super::super::vcs::git::gh_environment()),
        debug: false,
    };
    let account = github_quota::request_account_key(&request);
    let store = GithubQuotaStore::new(records.path().to_path_buf());
    let policy = GithubQuotaPolicy::default();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let reset = now + 1200;
    // 500 of 5,000 left is exactly the 10% background reserve.
    store.refresh_if_due(&policy, &account, now, &|| {
        RateLimitAnswer::Known(BTreeMap::from([(
            github_quota::RESOURCE_CORE.to_string(),
            ObservedQuota {
                limit: 5000,
                remaining: 500,
                reset_epoch_seconds: reset,
            },
        )]))
    });

    let error = github_quota::with_scoped_guard(store, policy, || {
        run_gh(scratch.path(), request, "gh run list")
    })
    .expect_err("the background reserve refuses the sweep's read");
    let throttle = error
        .private_vcs_failure()
        .and_then(|failure| failure.read_throttle.clone())
        .expect("typed throttle");
    assert_eq!(throttle.resource.as_deref(), Some("core"));
    assert_eq!(throttle.reset_epoch_seconds, Some(reset));
    assert!(gh.calls().is_empty(), "nothing may run: {:?}", gh.calls());
}
