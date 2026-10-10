#![deny(clippy::print_stderr, clippy::print_stdout)]
#![allow(missing_docs)]
#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used))]
//! Machine identity and workspace registry domain for Orbit.
//!
//! This crate owns machine identity, the logical workspace catalog, local
//! checkout bindings, owner-local task-publication repository bindings, the
//! operator's registered remote hosts, and their file persistence and
//! validation. It contains no command orchestration,
//! MCP transport, Core runtime execution, or shared database access.

pub mod hosts;
pub mod machine_identity;
pub mod workspace_registry;

#[cfg(test)]
mod tests;

pub use machine_identity::{
    MachineIdentity, MachineIdentityOutcome, MachineIdentityState, NewMachineIdentity,
    ensure_machine_identity, inspect_machine_identity, load_machine_identity, os_hostname,
};
