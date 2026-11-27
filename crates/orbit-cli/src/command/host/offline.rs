use std::path::Path;

use clap::Args;
use orbit_core::app::host::Command;

use super::support::run;
use super::target::HostShowArgs;
use crate::command::CommandOut;

#[derive(Args)]
pub struct HostOfflineArgs {
    #[command(flatten)]
    pub target: HostShowArgs,
    /// Reason for the planned downtime
    #[arg(long)]
    pub reason: String,
    /// Expire the acknowledgement at this RFC 3339 timestamp
    #[arg(long, value_name = "TIMESTAMP")]
    pub until: Option<String>,
}

pub(super) fn execute(args: HostOfflineArgs, global_root: &Path) -> CommandOut {
    run(Command::SetOffline {
        global_root: global_root.to_path_buf(),
        host: args.target.host,
        offline: Some((args.reason, args.until)),
    })
}
