//! v2 activity runtime. Phase 2 coexistence layer — the v1 runtime is untouched.
//!
//! Depends on `orbit_types::workflow::activity_job::activity_job` for the type surface (activity/spec/audit
//! shapes, tool-allowlist helpers). This module wires those types to the
//! engine's executor infrastructure and to the loop-engine audit pipeline.

pub mod asset_loader;
pub mod audit_writer;
pub mod catalog;
pub mod cli_runner;
pub mod crew;
pub mod dispatcher;
pub mod job_executor;
pub mod sqlite_sink;
pub mod tool_enforcement;
pub mod workspace;

#[cfg(test)]
mod tests;

pub use audit_writer::V2AuditWriter;
pub use crew::{ResolvedAgentSettings, inject_system_crew_input, resolve_crew_settings};
pub use dispatcher::{
    DispatchError, DispatchOutcome, LinuxRuntimeWriteAuthority, ResolvedCliExecutor,
    ResolvedSandbox, ResolvedShellExecutor, V2DispatchInput, dispatch_error_to_orbit,
    dispatch_v2_activity,
};
pub use job_executor::{
    JobOutcome, execute_job_with_resume, resolve_job_catalog_refs_for_execution, validate_job,
    validate_job_deterministic_actions,
};
pub use sqlite_sink::V2SqliteSink;
pub use tool_enforcement::EnforcedAuditSink;

pub use asset_loader::{
    ActivityAsset, AssetLoadError, JobAsset, load_activity_asset, load_job_asset,
};
pub use catalog::{
    ACTIVITY_REF_PREFIX, CatalogDirectory, CatalogDirectoryList, CatalogError, ResolveError,
    V2ActivityCatalog, V2JobCatalog, catalog_error_to_orbit, load_activity_catalog_asset,
    resolve_job_target_refs, validate_catalog_activity_tools,
};
