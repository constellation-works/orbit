//! The crate's boundaries: its dependency boundary and the MCP wire transport.
//!
//! One integration-test binary per area keeps link cost down; add a module
//! here rather than a new top-level `tests/*.rs` file
//! (`docs/design-patterns/test_strategy.md`).

// Integration fixtures unwrap setup invariants.
#![allow(clippy::expect_used, clippy::unwrap_used)]

#[cfg(unix)]
mod busy_owner_route;
mod capability;
mod dep_boundary;
mod federated_routing;
mod identity;
mod listener;
mod mcp_wire_roundtrip;
#[cfg(unix)]
mod oversized_lines;
mod scripted_destination;
#[cfg(unix)]
mod ssh_reuse;
mod task_route;
