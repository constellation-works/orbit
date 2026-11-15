//! `orbit web` — thin delegator to the `orbit-web` crate.
//!
//! The real implementation (ServeArgs, serve(), router, assets, API handlers)
//! lives in the `orbit-web` crate so that orbit-cli incremental builds
//! do not pay the axum dependency tax on every change.

use clap::{Args, Subcommand};
use orbit_core::OrbitRuntime;

use crate::command::{CommandOut, CommandOutput, Execute};

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

impl Execute for WebCommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        self.command.execute(runtime)
    }
}

#[derive(Subcommand)]
pub enum WebSubcommand {
    /// Run the Orbit dashboard
    Serve(orbit_web::ServeArgs),
    /// Open a remote workspace's dashboard over an SSH tunnel
    Connect(orbit_web::ConnectArgs),
}

impl Execute for WebSubcommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        match self {
            WebSubcommand::Serve(args) => {
                orbit_web::serve(runtime, args)?;
                Ok(CommandOutput::Silent)
            }
            // `connect` is a client-side tunnel helper; the workspace lives on
            // the remote, so it needs no local runtime — and no local root
            // (the `orbit web` dispatch that carries `--root` is
            // `dispatch_web`, which is the path `orbit web connect` takes).
            WebSubcommand::Connect(args) => {
                orbit_web::connect(args, None)?;
                Ok(CommandOutput::Silent)
            }
        }
    }
}
