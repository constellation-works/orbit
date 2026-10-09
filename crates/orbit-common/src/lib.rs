#![deny(clippy::print_stderr, clippy::print_stdout)]
#![allow(missing_docs)]
#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used))]
//! Shared mechanism crate for the Orbit workspace.
//!
//! Domain contracts live in `orbit-types`. This crate owns `OrbitError` and
//! responsibility-based helpers: governance, filesystem, process, storage,
//! protocol, observability, security, and text.

mod error;
pub mod fs;
pub mod governance;
pub mod migration;
pub mod model;
pub mod model_defaults;
pub mod observability;
pub mod process;
pub mod protocol;
pub mod security;
pub mod storage;
pub mod text;

pub mod test_env;

pub mod test_fixtures;

pub mod test_process;

pub use error::{
    ArtifactOrigin, ArtifactOriginMode, ClaimRefusalKind, DependencyNotDelivered, HostRegistryCode,
    NotFoundKind, OrbitError, RecoverableVcsConflict, SqliteContention, StorageLayer,
    WorkspaceClaimHeld,
};
pub use fs::task_io::task_artifact_from_source_file;
pub use model::pricing::derive_cost_usd;
pub use tracing;
