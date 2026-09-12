//! Shared scheduling domain: routines, auto-tasks and delivery/state
//! automation, from definition discovery through dispatch and coverage.
//!
//! Every host capability the domain needs — identity, paths, stores, task and
//! run lifecycle, authorization — arrives through [`host::AutomationHost`],
//! which Core implements for its runtime. Store owns durability. This crate
//! owns no clock, runtime or executor.
#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used))]
// Extracted legacy projections retain their existing documentation coverage.
#![allow(missing_docs)]

pub mod auto_tasks;
mod checkpoint;
pub mod consumers;
pub mod delivery;
mod error;
pub mod host;
pub mod members;
pub mod review;
pub mod routines;
pub mod source;

pub use error::{AutomationError, automation_error_to_orbit};
pub use host::{AutomationHost, RunOwnerLiveness};
