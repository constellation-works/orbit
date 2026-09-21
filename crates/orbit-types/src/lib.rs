#![deny(clippy::print_stderr, clippy::print_stdout)]
// Legacy domain-schema surfaces still need a focused documentation pass.
#![allow(missing_docs)]
#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used))]
#![allow(
    rustdoc::broken_intra_doc_links,
    rustdoc::invalid_html_tags,
    rustdoc::private_intra_doc_links
)]
//! Lowest internal Orbit contract crate.
//!
//! Domain-qualified modules only. `OrbitId` is the sole crate-root primitive.
//! This crate owns data structures, serde contracts, pure constructors,
//! normalization, lifecycle rules, and narrow domain errors. It does not
//! perform filesystem, process, environment, database, network, logging, or
//! tracing work and does not depend on another Orbit crate.

pub mod identity;
pub mod plugin;
pub mod policy;
pub mod record;
pub mod resource;
pub mod task;
pub mod telemetry;
pub mod tool;
pub mod workflow;
pub mod workspace;

pub use identity::OrbitId;
