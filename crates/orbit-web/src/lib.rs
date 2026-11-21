#![cfg_attr(not(test), deny(clippy::disallowed_methods))]
//! `orbit-web` — HTTP API, dashboard UI, and remote web connection.
//!
//! This crate isolates the axum-based dashboard (HTML/JS assets + `/api/*`
//! handlers) from orbit-cli so that CLI changes do not force rebuilds of the
//! large dependency subtree (axum, etc). Behavior is identical to the prior
//! in-tree implementation.
//!
//! Public entry points include `ServeArgs` (clap), `serve()` for a
//! caller-supplied runtime (single-workspace mode), and `serve_from_env()`,
//! the entry point for `orbit web serve`, which always serves every registered
//! workspace (global mode; see ORB-10029). [`lease_router`] exposes just the
//! lease API over a [`LeaseBackend`] for servers with their own authentication.
//! [`lease_intake`] is the Unix socket Orbit Web serves to the lease listener
//! user when there is an intake peer user, and the client the listener calls
//! it with. [`lease_listener`] is that listener: the lease routes and
//! `/healthz` behind lease-token authentication, run as the listener user.
//! Path leases are not configurable [ORB-22808], so Orbit Web serves no
//! intake socket and no command starts the listener.
//! All routes, content types, defaults, and graceful shutdown are preserved.

mod api;
mod assets;
mod connect;
mod doctor_report;
mod health;
mod heap;
mod host_tunnels;
#[cfg(unix)]
mod lease;
#[cfg(unix)]
pub use lease::{intake as lease_intake, listener as lease_listener};
mod log_format;
mod parse;
mod projections;
mod run_failure;
mod runtime_memo;
mod serve;
mod shutdown;
mod ssh_tunnel;
mod state;
mod why_stuck;

#[cfg(test)]
mod tests;

pub use api::leases::{
    LeaseBackend, LeaseBackendResult, LeaseCaller, LeaseCallerToken, LeaseNoticePages, lease_router,
};
pub use connect::{ConnectArgs, connect};

pub use serve::{ServeArgs, serve, serve_from_env};
