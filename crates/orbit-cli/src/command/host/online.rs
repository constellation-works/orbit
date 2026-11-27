use std::path::Path;

use orbit_core::app::host::Command;

use super::support::run;
use super::target::HostShowArgs;
use crate::command::CommandOut;

pub(super) fn execute(args: HostShowArgs, global_root: &Path) -> CommandOut {
    run(Command::SetOffline {
        global_root: global_root.to_path_buf(),
        host: args.host,
        offline: None,
    })
}
