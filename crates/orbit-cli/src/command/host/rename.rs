use std::path::Path;

use clap::Args;
use orbit_cmd::hosts;

use super::support::change_payload;
use crate::command::CommandOut;

#[derive(Args)]
pub struct HostRenameArgs {
    /// Host name (case-insensitive) or machine_id
    #[arg(value_name = "HOST")]
    pub host: String,
    /// The new name
    #[arg(value_name = "NEW_NAME")]
    pub new_name: String,
}

pub(super) fn execute(args: HostRenameArgs, global_root: &Path) -> CommandOut {
    change_payload(hosts::rename_host(global_root, &args.host, &args.new_name)?)
}
