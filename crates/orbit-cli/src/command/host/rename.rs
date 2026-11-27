use std::path::Path;

use clap::Args;
use orbit_core::app::host::Command;

use super::support::run;
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
    run(Command::Rename {
        global_root: global_root.to_path_buf(),
        host: args.host,
        new_name: args.new_name,
    })
}
