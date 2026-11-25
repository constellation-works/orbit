//! Concrete record repositories and cross-driver repositories.

pub(crate) mod admission_load;
pub(crate) mod application_consent;
pub(crate) mod backlog_forecast;
pub(crate) mod codebase_fact;
pub(crate) mod evolution;
mod file_backends;
pub(crate) mod friction;
pub(crate) mod landed_diff;
pub(crate) mod layered_policy;
pub(crate) mod notice;
pub(crate) mod recovery_authority;
pub(crate) mod sqlite_backends;
pub(crate) mod task;
