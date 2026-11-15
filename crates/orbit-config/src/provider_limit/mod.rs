//! When a provider's usage reading keeps delivery admission off its crews
//! (`workflow.provider_limit_*`).
//!
//! The readings themselves live in the host's provider-limit store. These
//! settings define when admission skips a provider's crews, what happens to an
//! explicitly crewed task, and the budgets declared in
//! `workflow.provider_limit_budgets`.

mod budget;
mod policy;
mod target;

pub use budget::{ProviderLimitBudget, ProviderLimitBudgetUnit};
pub use policy::{
    CrewSelection, PROVIDER_LIMIT_MAX_USED_PCT, ProviderLimitExplicitCrews, ProviderLimitPolicy,
    ProviderLimitThreshold, USAGE_PRESSURE_CAP, USAGE_PRESSURE_STRENGTH, UsagePressurePolicy,
};
pub use target::ProviderLimitTarget;

pub(crate) use budget::{admit_budgets, validate_budget_crews};
pub(crate) use policy::{
    admit_crew_selection, admit_explicit_crews, admit_overrides, stored_provider_limit_override,
    validate_override_crews,
};
