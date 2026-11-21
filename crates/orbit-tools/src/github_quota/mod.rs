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
//! A local refusal is the same typed [`orbit_common::ProviderReadThrottle`] GitHub's own
//! answer produces, naming the resource and the reset, so a caller that
//! already waits for a reset keeps working.
//!
//! The guard is off until the `orbit` binary enables it for the process
//! ([`enable_host_guard`]); library and test processes run `gh` unguarded.

mod account;
mod admission;
#[cfg(any(test, feature = "test-support"))]
mod fixture;
mod guard;
mod request;
mod store;

pub use account::{
    SharedReset, inherited_account_key, request_account_key, shared_exhausted_reset,
};
pub use admission::{
    admit_gh_request, admit_gh_request_within_budget, gh_request_deadline,
    observe_gh_failure_by_deadline, observe_gh_result_by_deadline, run_gh_request,
};
#[cfg(any(test, feature = "test-support"))]
pub use fixture::FixtureGh;
#[cfg(any(test, feature = "test-support"))]
pub use guard::with_scoped_guard;
pub use guard::{configure_host_policy, enable_host_guard, host_quota_status};
#[cfg(any(test, feature = "test-support"))]
pub use request::with_simulated_clipped_probe;
pub use store::{
    CallCounts, DEFAULT_AGENT_RESERVE_PERCENT, DEFAULT_BACKGROUND_RESERVE_PERCENT,
    DEFAULT_REFRESH_INTERVAL, GithubCaller, GithubQuotaPolicy, GithubQuotaStatus, GithubQuotaStore,
    GithubResourceStatus, ObservedQuota, QuotaRefusal, QuotaRefusalReason, RESOURCE_CORE,
    RESOURCE_GRAPHQL, RateLimitAnswer,
};
#[cfg(any(test, feature = "test-support"))]
pub use store::{
    advance_deadline_clock_after_next_refusal_decision, advance_deadline_clock_at_next_refusal,
};

use admission::remaining_millis;
