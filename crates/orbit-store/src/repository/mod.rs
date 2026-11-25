//! Concrete record repositories and cross-driver repositories.

pub(crate) mod admission_load;
pub(crate) mod application_consent;
pub(crate) mod backlog_forecast;
pub(crate) mod codebase_fact;
pub(crate) mod crew_memory;
pub(crate) mod evolution;
pub(crate) mod experiment;
mod file_backends;
pub(crate) mod friction;
pub(crate) mod landed_diff;
pub(crate) mod layered_policy;
pub(crate) mod notice;
pub(crate) mod provider_limit;
pub(crate) mod recovery_authority;
pub(crate) mod reliability;
pub(crate) mod resource_usage;
pub(crate) mod retention;
pub(crate) mod sqlite_backends;
pub(crate) mod task;
