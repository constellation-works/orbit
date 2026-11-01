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
pub mod script_catalog;
pub mod sqlite_sink;
pub mod workspace;

pub use audit_writer::V2AuditWriter;
pub use crew::{ResolvedAgentSettings, resolve_crew_settings};
pub use dispatcher::{
    DispatchError, DispatchInvocationTrace, DispatchOutcome, LinuxRuntimeWriteAuthority,
    ResolvedCliExecutor, ResolvedSandbox, ResolvedShellExecutor, SandboxMask, V2DispatchInput,
    dispatch_error_to_orbit, dispatch_v2_activity,
};
pub use job_executor::{
    JobOutcome, execute_job_with_resume, resolve_job_catalog_refs_for_execution,
    resolve_job_catalog_refs_with_scripts_for_execution, validate_job,
    validate_job_deterministic_actions,
};
pub use sqlite_sink::V2SqliteSink;

pub use asset_loader::{
    ActivityAsset, AssetLoadError, JobAsset, load_activity_asset, load_job_asset,
};
pub use catalog::{
    ACTIVITY_REF_PREFIX, CatalogDirectory, CatalogDirectoryList, CatalogError, ResolveError,
    SCRIPT_REF_PREFIX, V2ActivityCatalog, V2JobCatalog, catalog_error_to_orbit,
    job_definition_files, load_activity_catalog_asset, resolve_job_target_refs,
    resolve_job_target_refs_with_scripts, validate_catalog_activity_tools,
};
pub use script_catalog::{
    ScriptArgError, ScriptAsset, ScriptLoadError, ScriptSnapshot, V2ScriptCatalog,
    load_script_asset, load_script_snapshot, render_script_args, script_stdout_output,
};
