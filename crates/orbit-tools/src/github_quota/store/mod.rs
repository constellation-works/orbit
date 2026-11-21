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
//! The store's own failures never refuse a read on their own. When a record's
//! update fails on a lock or write error but the record still reads cleanly,
//! the decision uses a read-only copy: an active hold or the reserve refuses
//! the read as it otherwise would, uncounted. An admitted read is not counted
//! or debited, and the record file is not written. A record that cannot be
//! read (an I/O error or malformed JSON) is preserved untouched and admits the
//! read, uncounted.

mod clock;
mod quota_store;
mod record;
mod types;

#[cfg(any(test, feature = "test-support"))]
pub(super) use clock::reach_deadline;
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
