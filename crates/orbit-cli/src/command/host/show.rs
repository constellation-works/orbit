use std::fmt::Write as _;
use std::path::Path;

use orbit_cmd::hosts::{self, HostDetail};

use super::support::{host_block, to_json};
use super::target::HostShowArgs;
use crate::command::{CommandOut, Payload};

pub(super) fn execute(args: HostShowArgs, global_root: &Path) -> CommandOut {
    show_payload(hosts::show_host(global_root, &args.host)?)
}

fn show_payload(detail: HostDetail) -> CommandOut {
    let mut text = host_block(&detail.host);
    if let Some(dependents) = &detail.dependents {
        let lines = dependents.describe();
        if lines.is_empty() {
            text.push_str("  depended on by: nothing on this machine\n");
        } else {
            text.push_str("  depended on by:\n");
            for line in lines {
                let _ = writeln!(text, "    {line}");
            }
        }
    }
    Ok(Payload::detail(to_json(&detail)?, text).into())
}
