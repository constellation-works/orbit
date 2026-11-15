//! Read-only operator attention: local scopes, source derivation and the
//! destination side of federation. Filters, the cursor and the public page
//! live with the operation in `orbit_common::governance::attention`, so the
//! federated aggregator applies the same rules once over every host.
mod host;
mod notice_sources;
mod provider_sources;
mod query;
mod runtime_sources;
mod sources;

pub(crate) use query::attention_query;
use query::{
    ATTENTION_AGE_BUMP_HOURS, ATTENTION_CI_CACHE_SECONDS, ATTENTION_RUN_STUCK_AFTER_MINUTES,
    ATTENTION_RUN_WINDOW_HOURS, ATTENTION_SOURCE_TIMEOUT_SECONDS,
};
