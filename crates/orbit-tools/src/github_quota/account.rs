use super::admission::now_epoch_seconds;
use super::guard::active_guard;
use super::{request, *};
use orbit_exec::ExecRequest;
use std::time::Instant;
/// The opaque record key for the GitHub host and credential `request` uses.
pub fn request_account_key(request: &ExecRequest) -> String {
    request::request_account(request)
}

/// The record key a `gh` call inheriting this process's environment uses.
pub fn inherited_account_key() -> String {
    request::request_account(&crate::gh_request::gh_exec_request(Vec::new(), None, 0))
}

/// The reset a completion-style caller waits for, from the shared record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SharedReset {
    /// The guard is off: the caller reads the endpoint itself.
    Unguarded,
    /// The record's latest exhausted reset among the asked resources, if any.
    Guarded(Option<u64>),
}

/// The latest exhausted reset of `resources` for the account `request` uses,
/// read through the host-shared record. The rate-limit endpoint is read only
/// when no process on the host has within the refresh interval, so a lookup
/// right after an admission probe does not call it again. `request` supplies
/// the program, environment, directory and timeout budget of that read; that
/// budget also bounds every wait for the record's lock.
pub fn shared_exhausted_reset(request: &ExecRequest, resources: &[&str]) -> SharedReset {
    let Some((store, policy)) = active_guard() else {
        return SharedReset::Unguarded;
    };
    let account = request::request_account(request);
    let deadline = gh_request_deadline(request, Instant::now());
    SharedReset::Guarded(store.bounded_by(deadline).exhausted_reset(
        &policy,
        &account,
        resources,
        now_epoch_seconds(),
        &|| request::read_rate_limit(request, deadline),
    ))
}
