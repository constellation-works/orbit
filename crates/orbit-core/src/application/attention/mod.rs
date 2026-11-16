//! Read-only operator attention: local scopes, source derivation and the
//! destination side of federation. Filters, the cursor and the public page
//! live with the operation in `orbit_common::governance::attention`, so the
//! federated aggregator applies the same rules once over every host.
mod host;
mod notice_sources;
mod provider_sources;
mod query;
mod routine_sources;
mod runtime_sources;
mod sources;
mod wait;
mod wait_grant;

pub(crate) use query::attention_query;
use query::{
    ATTENTION_AGE_BUMP_HOURS, ATTENTION_CI_CACHE_SECONDS, ATTENTION_RUN_STUCK_AFTER_MINUTES,
    ATTENTION_RUN_WINDOW_HOURS, ATTENTION_SOURCE_TIMEOUT_SECONDS,
};
pub(crate) use wait::wait_for_attention_delta;
pub use wait::{AttentionWaitOutcome, AttentionWaitStatus};
pub(crate) use wait_grant::AttentionWaitScope;
pub(crate) use wait_grant::with_wait_grant;
