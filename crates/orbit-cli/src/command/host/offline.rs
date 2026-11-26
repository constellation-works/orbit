use std::path::Path;

use clap::Args;
use orbit_cmd::hosts;

use super::support::change_payload;
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
    change_payload(hosts::set_host_offline(
        global_root,
        &args.target.host,
        Some((&args.reason, args.until.as_deref())),
    )?)
}
