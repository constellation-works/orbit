//! The task PR forge's `gh` reads under the host's GitHub quota [ORB-25868]:
//! once a rate-limit answer holds `graphql`, a forge read is refused with the
//! typed throttle and runs nothing.

use orbit_exec::{EnvironmentMode, ExecRequest, StdinMode};
use orbit_tools::github_quota::{self, FixtureGh, GithubQuotaPolicy, GithubQuotaStore};
use tempfile::TempDir;

use super::super::pr_forge::task_forge_gh_request;

#[test]
fn held_forge_read_is_refused_typed_and_runs_nothing() {
    let scratch = TempDir::new().unwrap();
    let records = TempDir::new().unwrap();
    let gh = FixtureGh::create(scratch.path(), false).unwrap();
    let request = ExecRequest {
        program: gh.program(),
        args: vec!["pr".into(), "list".into(), "--json".into(), "number".into()],
        current_dir: None,
        timeout_ms: Some(10_000),
        stdin_mode: StdinMode::Null,
        environment_mode: EnvironmentMode::Inherit,
        debug: false,
    };
    let account = github_quota::request_account_key(&request);
    let store = GithubQuotaStore::new(records.path().to_path_buf());
    let policy = GithubQuotaPolicy::default();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let reset = now + 600;
    store.record_throttle(
        &policy,
        &account,
        github_quota::RESOURCE_GRAPHQL,
        &orbit_common::ProviderReadThrottle {
            reset_epoch_seconds: Some(reset),
            principal: Some("user ID 42".into()),
            ..Default::default()
        },
        now,
    );

    let error = github_quota::with_scoped_guard(store, policy, || {
        task_forge_gh_request(request, scratch.path(), "gh pr list")
    })
    .expect_err("a held forge read must be refused");
    let throttle = error
        .private_vcs_failure()
        .and_then(|failure| failure.read_throttle.clone())
        .expect("typed throttle");
    assert_eq!(throttle.resource.as_deref(), Some("graphql"));
    assert_eq!(throttle.reset_epoch_seconds, Some(reset));
    // Only the free rate-limit endpoint may be read; the refused read never runs.
    assert_eq!(gh.calls(), vec!["api rate_limit".to_string()]);
}
