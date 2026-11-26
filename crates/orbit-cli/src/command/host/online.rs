use std::path::Path;

use orbit_cmd::hosts;

use super::support::change_payload;
use super::target::HostShowArgs;
use crate::command::CommandOut;

pub(super) fn execute(args: HostShowArgs, global_root: &Path) -> CommandOut {
    change_payload(hosts::set_host_offline(global_root, &args.host, None)?)
}
