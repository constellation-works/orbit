use std::path::Path;

use clap::Args;
use orbit_core::app::host::Command;

use super::support::run;
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
    run(Command::Remove {
        global_root: global_root.to_path_buf(),
        host: args.host,
        force: args.force,
    })
}
