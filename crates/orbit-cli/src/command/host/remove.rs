use std::path::Path;

use clap::Args;
use orbit_cmd::hosts;

use super::support::change_payload;
use crate::command::CommandOut;

#[derive(Args)]
pub struct HostRemoveArgs {
    /// Host name (case-insensitive) or machine_id
    #[arg(value_name = "HOST")]
    pub host: String,
    /// Remove the entry even while a replica checkout or pull drain routes to it
    #[arg(long)]
    pub force: bool,
}

pub(super) fn execute(args: HostRemoveArgs, global_root: &Path) -> CommandOut {
    change_payload(hosts::remove_host(global_root, &args.host, args.force)?)
}
