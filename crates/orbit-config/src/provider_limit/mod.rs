//! When a provider's usage reading keeps delivery admission off its crews
//! (`workflow.provider_limit_*`).
//!
//! The readings themselves live in the host's provider-limit store; these
//! settings only say how close to a window's limit a provider may run before
//! admission skips its crews, and what an explicitly crewed task does then.

mod budget;
mod policy;
mod target;

pub use budget::{ProviderLimitBudget, ProviderLimitBudgetUnit};
pub use policy::{
    CrewSelection, DEFAULT_PROVIDER_LIMIT_MAX_USED_PCT, DEFAULT_USAGE_PRESSURE_CAP,
    DEFAULT_USAGE_PRESSURE_STRENGTH, ProviderLimitExplicitCrews, ProviderLimitPolicy,
    ProviderLimitThreshold, UsagePressurePolicy,
};
pub use target::ProviderLimitTarget;

pub(crate) use budget::{admit_budgets, validate_budget_crews};
pub(crate) use policy::{
    admit_crew_selection, admit_explicit_crews, admit_overrides, admit_usage_pressure_cap,
    admit_usage_pressure_strength, validate_override_crews,
};
