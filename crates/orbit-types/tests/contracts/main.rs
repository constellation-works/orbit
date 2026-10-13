//! Pure domain contracts exercised through the exported types API.
//!
//! Plugin, task, tool and workflow cases share one binary to pay one link cost;
//! workspace publication cases use the existing workspace integration binary.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod plugin;
mod task;
mod tool;
mod workflow;
