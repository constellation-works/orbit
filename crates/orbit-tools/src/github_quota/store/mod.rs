//! The host-shared GitHub quota record and its admission decision.
//!
//! One JSON file per GitHub account under the store directory holds, per
//! resource (`core`, `graphql`), what GitHub's rate-limit endpoint last
//! reported, a local hold after an authenticated primary rate-limit answer,
//! and the calls each caller class made in the current window. Every change
//! is a read-modify-write under an exclusive file lock beside the record and
//! lands through a staging file and an atomic rename, so concurrent writers in
//! any number of processes never leave a torn record.
//!
//! The store never turns its own failure into a refusal: an unreadable or
//! unwritable record admits the read.

mod clock;
mod quota_store;
mod record;
mod types;

#[cfg(any(test, feature = "test-support"))]
pub use clock::{
    advance_deadline_clock_after_next_refusal_decision, advance_deadline_clock_at_next_refusal,
};
pub(super) use clock::{budget_spent, monotonic_now};
pub use quota_store::GithubQuotaStore;
pub(super) use record::rfc3339;
pub use types::{
    CallCounts, DEFAULT_AGENT_RESERVE_PERCENT, DEFAULT_BACKGROUND_RESERVE_PERCENT,
    DEFAULT_REFRESH_INTERVAL, GithubCaller, GithubQuotaPolicy, GithubQuotaStatus,
    GithubResourceStatus, ObservedQuota, QuotaRefusal, QuotaRefusalReason, RESOURCE_CORE,
    RESOURCE_GRAPHQL, RateLimitAnswer,
};
