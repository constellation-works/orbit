//! Domain contracts for this Orbit types module.

mod audit_actor;
mod audit_event;
mod invocation;
mod metrics;
mod pricing;
mod provider_limit;
mod self_reported_actor;

pub use audit_actor::{
    ACTOR_ALIAS_MAP_VERSION, ActorKind, CanonicalActor, canonical_actor_for_role_label,
};
pub use audit_event::{AuditEvent, AuditEventStatus, AuditStats};
pub use invocation::{InvocationTrace, TokenUsage, ToolCallTrace};
pub use metrics::MetricsEntry;
pub use pricing::{InputTokenBasis, PriceRow, cost_from_rows, normalize_token_usage_from_rows};
pub use provider_limit::{
    PROVIDER_LIMIT_DETAIL_MAX_BYTES, ProviderLimitObservation, ProviderLimitSource,
    USAGE_REPORTING_PROVIDERS,
};
pub use self_reported_actor::{
    ANONYMOUS_ACTOR_LABEL, AuditAttribution, SELF_REPORTED_ACTOR_MAX_LEN,
    normalize_self_reported_actor,
};
