//! Initialization, managed asset seeding, and forward-only startup migrations.

pub(crate) mod activity;
pub(crate) mod global_defaults;
pub mod init;
/// Host capability probe shared by explicit onboarding and read-only diagnosis.
/// Exec owns the probe shape; Core exposes it to command surfaces.
pub mod linux_sandbox_host {
    #[cfg(target_os = "linux")]
    pub use orbit_exec::probe_bwrap_fresh_for_user;
    pub use orbit_exec::{
        BUNDLED_BWRAP_PATH, BUNDLED_BWRAP_VERSION, BwrapProbeOutcome, BwrapSource,
        probe_bwrap_fresh,
    };
}
pub(crate) mod policy;
pub(crate) mod product_profile;
pub mod task_migration;
pub mod task_publication;

#[cfg(test)]
mod tests;
