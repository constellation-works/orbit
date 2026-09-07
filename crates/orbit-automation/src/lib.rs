//! Shared scheduling domain. Core supplies sources and lifecycle adapters;
//! Store owns durability. This crate owns no clock, runtime or executor.
#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used))]
// Extracted legacy projections retain their existing documentation coverage.
#![allow(missing_docs)]

pub mod auto_tasks;
mod checkpoint;
pub mod delivery;
mod error;
pub mod members;
pub mod review;
pub mod routines;

pub use error::{AutomationError, automation_error_to_orbit};
