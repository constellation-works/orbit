use std::path::PathBuf;

use orbit_common::OrbitError;
use orbit_types::workflow::ToolAllowlistError;
use thiserror::Error;

use super::super::asset_loader::AssetLoadError;

#[derive(Debug, Error)]
pub enum CatalogError {
    #[error("read dir {path}: {source}")]
    ReadDir {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("read file {path}: {source}")]
    ReadFile {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("parse {path}: {source}")]
    Parse {
        path: PathBuf,
        source: AssetLoadError,
    },
    #[error("duplicate activity name `{name}` — defined in both {first} and {second}")]
    DuplicateName {
        name: String,
        first: PathBuf,
        second: PathBuf,
    },
    #[error("duplicate v2 job name '{name}' — defined in both {first} and {second}")]
    DuplicateJobName {
        name: String,
        first: PathBuf,
        second: PathBuf,
    },
    #[error("activity `{name}` tool allowlist invalid: {source}")]
    ToolAllowlist {
        name: String,
        source: ToolAllowlistError,
    },
}

/// Translate catalog-loading failures at the orbit-engine crate boundary.
pub fn catalog_error_to_orbit(error: CatalogError) -> OrbitError {
    OrbitError::InvalidInput(error.to_string())
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ResolveError {
    #[error(
        "step `{step_id}`: target `{target}` does not start with `activity:`, `script:` or `auto-task:`"
    )]
    UnknownRefKind { step_id: String, target: String },
    #[error("step `{step_id}`: activity `{name}` not found in catalog")]
    ActivityNotInCatalog { step_id: String, name: String },
    #[error("{referrer}activity `{name}` is unavailable: {reason}")]
    ActivityUnavailable {
        /// Prefix naming where the reference sits: a step id or a job field.
        referrer: String,
        name: String,
        reason: String,
    },
    #[error("step `{step_id}`: script `{name}` is unavailable: {reason}")]
    ScriptUnavailable {
        step_id: String,
        name: String,
        reason: String,
    },
    #[error("job recovery_activity `{name}` not found in catalog")]
    RecoveryActivityNotInCatalog { name: String },
    #[error("job failure_activity `{name}` not found in catalog")]
    FailureActivityNotInCatalog { name: String },
    #[error("job final_recovery_activity `{name}` not found in catalog")]
    FinalRecoveryActivityNotInCatalog { name: String },
    #[error("step `{step_id}`: recovery_activity `{name}` not found in catalog")]
    StepRecoveryActivityNotInCatalog { step_id: String, name: String },
    #[error(
        "step `{step_id}`: script `{name}` manifest not found at {manifest}; a `script:` step names a manifest in the workspace's scripts directory"
    )]
    ScriptNotInCatalog {
        step_id: String,
        name: String,
        manifest: PathBuf,
    },
}
