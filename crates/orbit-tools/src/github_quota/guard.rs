use super::admission::now_epoch_seconds;
use super::{request, *};
use std::sync::RwLock;
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

pub(super) fn active_guard() -> Option<(GithubQuotaStore, GithubQuotaPolicy)> {
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

/// The record for the account a default `gh` call in this process uses,
/// refreshed first when due; `None` while the guard is off.
pub fn host_quota_status() -> Option<GithubQuotaStatus> {
    let (store, policy) = active_guard()?;
    let mut probe = crate::gh_request::gh_exec_request(Vec::new(), None, 0);
    // Status inspection has no caller deadline; use the full refresh timeout.
    probe.timeout_ms = None;
    let account = request::request_account(&probe);
    let now = now_epoch_seconds();
    store.refresh_if_due(&policy, &account, now, &|| {
        request::read_rate_limit(&probe, None)
    });
    Some(store.status(&policy, &account, now))
}
