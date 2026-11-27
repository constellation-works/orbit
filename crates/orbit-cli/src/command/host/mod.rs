//! `orbit host` — the remote Orbit hosts this machine reaches over SSH.
//!
//! Each command loads `~/.orbit/hosts.toml` through `orbit_core::app::host`, which
//! the dashboard also calls, and opens no workspace runtime. Reachability,
//! version, protocol and workspaces are read from each host live and never
//! persisted. The registry stores identity, routing and offline acknowledgements.
//! The optional OS snapshot is read from the
//! host when it is added, so entries registered before discovery
//! reported an OS have none until they are removed and added again.

mod add;
mod command;
mod list;
mod offline;
mod online;
mod remove;
mod rename;
mod show;
mod support;
mod target;

pub use command::{HostCommand, HostSubcommand};
pub(crate) use support::acknowledged_host_lines;
