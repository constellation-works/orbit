use std::path::Path;

use clap::Args;
use orbit_core::app::host::Command;

use super::support::run;
use crate::command::CommandOut;

#[derive(Args)]
#[command(
    after_help = "Examples:\n  orbit host add dk-server-2\n  orbit host add daniel@10.0.0.7 --name build-box\n\n\
        A host already in hosts.toml refuses with host_exists."
)]
pub struct HostAddArgs {
    /// SSH alias or user@host that reaches the remote host
    #[arg(value_name = "SSH_TARGET")]
    pub ssh_target: String,
    /// Name for the host (defaults to the remote's own machine.name)
    #[arg(long)]
    pub name: Option<String>,
}

pub(super) fn execute(args: HostAddArgs, global_root: &Path) -> CommandOut {
    run(Command::Add {
        global_root: global_root.to_path_buf(),
        ssh: args.ssh_target,
        name: args.name,
    })
}
