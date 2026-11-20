//! The host's GitHub quota guard [ORB-25868].
//!
//! Every place Orbit executes `gh` against the API asks this module first:
//! the `github.*` tools (agent reads), the engine's private VCS operations
//! and the task PR forge (delivery), and the engine sweeps and auto-task
//! sources (background). The guard keeps one host-shared record per GitHub
//! account and resource ([`GithubQuotaStore`]), refreshed from GitHub's free
//! rate-limit endpoint at most once per interval, and refuses a read locally
//! when:
//!
//! - an authenticated primary rate-limit answer (or an empty bucket) holds
//!   every read of that resource until its reset; or
//! - the remaining count is at or below the caller class's reserve (agent
//!   20% and background 10% of the limit by default; delivery none).
//!
//! A local refusal is the same typed [`ProviderReadThrottle`] GitHub's own
//! answer produces, naming the resource and the reset, so a caller that
//! already waits for a reset keeps working.
//!
//! The guard is off until the `orbit` binary enables it for the process
//! ([`enable_host_guard`]); library and test processes run `gh` unguarded.

#[cfg(any(test, feature = "test-support"))]
mod fixture;
mod request;
mod store;

#[cfg(any(test, feature = "test-support"))]
pub use fixture::FixtureGh;

use std::sync::RwLock;
use std::time::{SystemTime, UNIX_EPOCH};

use orbit_common::{OrbitError, PrivateVcsFailure, PrivateVcsFailureClass, ProviderReadThrottle};
use orbit_exec::{ExecRequest, NoSandbox, run_process};
use orbit_types::tool::ExecutionResult;

pub use store::{
    CallCounts, DEFAULT_AGENT_RESERVE_PERCENT, DEFAULT_BACKGROUND_RESERVE_PERCENT,
    DEFAULT_REFRESH_INTERVAL, GithubCaller, GithubQuotaPolicy, GithubQuotaStatus, GithubQuotaStore,
    GithubResourceStatus, ObservedQuota, QuotaRefusal, QuotaRefusalReason, RESOURCE_CORE,
    RESOURCE_GRAPHQL, RateLimitAnswer,
};

/// The process's guard: `None` until [`enable_host_guard`].
static HOST_STORE: RwLock<Option<GithubQuotaStore>> = RwLock::new(None);
static HOST_POLICY: RwLock<Option<GithubQuotaPolicy>> = RwLock::new(None);

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static SCOPED_GUARD: std::cell::RefCell<Option<(GithubQuotaStore, GithubQuotaPolicy)>> =
        const { std::cell::RefCell::new(None) };
}

/// Guard every `gh` read this process makes with the host-shared record under
/// the Orbit data root. Called once by the `orbit` binary.
pub fn enable_host_guard() {
    let Some(store) = GithubQuotaStore::in_data_root() else {
        return;
    };
    if let Ok(mut slot) = HOST_STORE.write() {
        *slot = Some(store);
    }
}

/// Apply the configured reserves and refresh interval.
pub fn configure_host_policy(policy: GithubQuotaPolicy) {
    if let Ok(mut slot) = HOST_POLICY.write() {
        *slot = Some(policy);
    }
}

/// Run `body` on this thread with `store` and `policy` as the guard, whatever
/// the process installed. Test fixtures only.
#[cfg(any(test, feature = "test-support"))]
pub fn with_scoped_guard<T>(
    store: GithubQuotaStore,
    policy: GithubQuotaPolicy,
    body: impl FnOnce() -> T,
) -> T {
    struct Reset(Option<(GithubQuotaStore, GithubQuotaPolicy)>);
    impl Drop for Reset {
        fn drop(&mut self) {
            let previous = self.0.take();
            SCOPED_GUARD.with(|slot| *slot.borrow_mut() = previous);
        }
    }
    let previous = SCOPED_GUARD.with(|slot| slot.borrow_mut().replace((store, policy)));
    let _reset = Reset(previous);
    body()
}

fn active_guard() -> Option<(GithubQuotaStore, GithubQuotaPolicy)> {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(scoped) = SCOPED_GUARD.with(|slot| slot.borrow().clone()) {
        return Some(scoped);
    }
    let store = HOST_STORE.read().ok()?.clone()?;
    let policy = HOST_POLICY
        .read()
        .ok()
        .and_then(|slot| *slot)
        .unwrap_or_default();
    Some((store, policy))
}

fn now_epoch_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// Admit `request` for `caller`, or refuse it locally with a typed throttle.
/// A request that spends no API quota, and every request while the guard is
/// off, is admitted.
pub fn admit_gh_request(caller: GithubCaller, request: &ExecRequest) -> Result<(), OrbitError> {
    let Some((store, policy)) = active_guard() else {
        return Ok(());
    };
    let Some(resource) = request::request_resource(request) else {
        return Ok(());
    };
    let account = request::request_account(request);
    store
        .admit(
            &policy,
            &account,
            caller,
            resource,
            now_epoch_seconds(),
            &|| request::read_rate_limit(request),
        )
        .map_err(|refusal| refusal_error(&refusal, request))
}

/// Record what `result` of an admitted `request` says about the quota: an
/// authenticated primary rate-limit answer holds every read of its resource
/// on the host until the reset.
pub fn observe_gh_result(request: &ExecRequest, result: &ExecutionResult) {
    if !result.success {
        observe_gh_failure(request, &result.stderr);
    }
}

/// [`observe_gh_result`] for a failed `request` whose diagnostic is `stderr`.
pub fn observe_gh_failure(request: &ExecRequest, stderr: &str) {
    let Some((store, policy)) = active_guard() else {
        return;
    };
    let Some(resource) = request::request_resource(request) else {
        return;
    };
    let Some(throttle) = crate::builtin::github::throttle::read_throttle(stderr) else {
        return;
    };
    // A secondary limit names no account; it throttles one request pattern,
    // not the host's bucket.
    if throttle.principal.is_none() {
        return;
    }
    let resource = throttle
        .resource
        .as_deref()
        .filter(|named| *named == RESOURCE_CORE || *named == RESOURCE_GRAPHQL)
        .unwrap_or(resource);
    store.record_throttle(
        &policy,
        &request::request_account(request),
        resource,
        &throttle,
        now_epoch_seconds(),
    );
}

/// Remaining timeout budget after `elapsed`, or `None` when `request`'s
/// finite budget is already exhausted.
fn remaining_request_budget(
    request: &ExecRequest,
    elapsed: std::time::Duration,
) -> Option<ExecRequest> {
    let Some(budget_ms) = request.timeout_ms else {
        return Some(request.clone());
    };
    let elapsed_ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
    let remaining_ms = budget_ms.saturating_sub(elapsed_ms);
    if remaining_ms == 0 {
        return None;
    }
    let mut bounded = request.clone();
    bounded.timeout_ms = Some(remaining_ms);
    Some(bounded)
}

fn timed_out_before_start(duration_ms: u64) -> ExecutionResult {
    ExecutionResult {
        success: false,
        timed_out: true,
        stdout: String::new(),
        stderr: "process timed out before starting".to_string(),
        exit_code: None,
        duration_ms,
        output: None,
    }
}

/// [`admit_gh_request`], run `request` without a sandbox with the remaining
/// timeout budget, then [`observe_gh_result`].
pub fn run_gh_request(
    caller: GithubCaller,
    request: &ExecRequest,
) -> Result<ExecutionResult, OrbitError> {
    if request.timeout_ms == Some(0) {
        return Ok(timed_out_before_start(0));
    }
    let started = std::time::Instant::now();
    admit_gh_request(caller, request)?;
    let elapsed = started.elapsed();
    let Some(bounded_request) = remaining_request_budget(request, elapsed) else {
        return Ok(timed_out_before_start(
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
        ));
    };
    let result = run_process(&bounded_request, &NoSandbox)?;
    observe_gh_result(&bounded_request, &result);
    Ok(result)
}

/// The opaque record key for the GitHub host and credential `request` uses.
pub fn request_account_key(request: &ExecRequest) -> String {
    request::request_account(request)
}

/// The record key a `gh` call inheriting this process's environment uses.
pub fn inherited_account_key() -> String {
    request::request_account(&crate::builtin::github::gh_exec_request(
        Vec::new(),
        None,
        0,
    ))
}

/// The record for the account a default `gh` call in this process uses,
/// refreshed first when due; `None` while the guard is off.
pub fn host_quota_status() -> Option<GithubQuotaStatus> {
    let (store, policy) = active_guard()?;
    let mut probe = crate::builtin::github::gh_exec_request(Vec::new(), None, 0);
    // Status inspection has no caller deadline; use the full refresh timeout.
    probe.timeout_ms = None;
    let account = request::request_account(&probe);
    let now = now_epoch_seconds();
    store.refresh_if_due(&policy, &account, now, &|| request::read_rate_limit(&probe));
    Some(store.status(&policy, &account, now))
}

/// The typed throttle a local refusal carries: the shape of GitHub's own
/// answer, so a caller that waits for a reset keeps working.
fn refusal_error(refusal: &QuotaRefusal, request: &ExecRequest) -> OrbitError {
    let resume = store::rfc3339(refusal.resume_epoch_seconds)
        .unwrap_or_else(|| refusal.resume_epoch_seconds.to_string());
    let why = match refusal.reason {
        QuotaRefusalReason::Exhausted => format!(
            "GitHub's `{}` API quota for this host's account is exhausted",
            refusal.resource
        ),
        QuotaRefusalReason::Reserve => format!(
            "GitHub's `{}` API quota for this host's account is down to {} of {} calls, \
             within the {} reserve of {} calls kept for delivery",
            refusal.resource,
            refusal.remaining.unwrap_or_default(),
            refusal.limit.unwrap_or_default(),
            refusal.caller.as_str(),
            refusal.reserve
        ),
    };
    let operation = request
        .args
        .iter()
        .take(2)
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(" ");
    let operation = format!("gh {operation}");
    let now = now_epoch_seconds();
    OrbitError::PrivateVcs(Box::new(PrivateVcsFailure {
        message: format!(
            "{operation} refused locally without calling GitHub: {why}. Reads of `{}` may \
             resume at {resume}; do not retry before then.",
            refusal.resource
        ),
        operation,
        class: PrivateVcsFailureClass::Other,
        read_throttle: Some(ProviderReadThrottle {
            retry_after_seconds: Some(refusal.resume_epoch_seconds.saturating_sub(now)),
            reset_epoch_seconds: Some(refusal.resume_epoch_seconds),
            resource: Some(refusal.resource.clone()),
            principal: None,
            request_id: None,
        }),
    }))
}
