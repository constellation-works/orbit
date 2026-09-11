#![deny(clippy::print_stderr, clippy::print_stdout)]
#![allow(missing_docs)]
#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used))]
#![allow(
    rustdoc::broken_intra_doc_links,
    rustdoc::invalid_html_tags,
    rustdoc::private_intra_doc_links
)]
//! Shared mechanism crate for the Orbit workspace.
//!
//! Domain contracts live in `orbit-types`. This crate owns `OrbitError` and
//! responsibility-based helpers: governance, filesystem, process, storage,
//! protocol, observability, and security.

pub mod error;
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

#[cfg(any(test, feature = "test-util"))]
pub mod test_env;

#[cfg(any(test, feature = "test-util"))]
pub mod test_fixtures;

#[cfg(any(test, feature = "test-util"))]
pub mod test_process;

pub use error::{
    ArtifactOrigin, ArtifactOriginMode, DependencyNotDelivered, FrictionNotLocal, NotFoundKind,
    OrbitError, RecoverableVcsConflict, WorkspaceClaimHeld,
};
pub use fs::task_io::{prune_missing_context_files, task_artifact_from_source_file};
pub use model::pricing::{derive_cost_usd, normalize_token_usage};
pub use observability::audit_id::audit_execution_id;
pub use protocol::tool_input;
pub use protocol::tool_schema;
pub use protocol::yaml::{
    parse_auto_task_yaml, parse_local_routine_yaml, parse_policy_resource, parse_routine_yaml,
    parse_task_plan,
};
pub use tracing;
