//! `orbit web` — thin delegator to the `orbit-web` crate.
//!
//! The real implementation (ServeArgs, serve(), router, assets, API handlers)
//! lives in the `orbit-web` crate so that orbit-cli incremental builds
//! do not pay the axum dependency tax on every change.

use clap::{Args, Subcommand};

/// Thin wrapper so `orbit web` continues to work and audit events stay stable.
#[derive(Args)]
#[command(
    about = "Run the Orbit dashboard",
    arg_required_else_help = true,
    subcommand_required = true
)]
pub struct WebCommand {
    #[command(subcommand)]
    pub command: WebSubcommand,
}

#[derive(Subcommand)]
pub enum WebSubcommand {
    /// Run the Orbit dashboard
    Serve(orbit_web::ServeArgs),
    /// Open a remote workspace's dashboard over an SSH tunnel
    Connect(orbit_web::ConnectArgs),
}
