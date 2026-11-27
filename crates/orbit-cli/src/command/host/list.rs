use std::path::Path;

use clap::Args;
use orbit_core::app::host::{Command, HostList};

use super::support::{UNKNOWN, marked_name, reachability, run, to_json, workspace_role};
use crate::command::{CommandOut, Payload};
use crate::output::common::table::{Column, Table};

#[derive(Args)]
pub struct HostListArgs {
    /// Print the stored fields only, without contacting any host
    #[arg(long)]
    pub no_probe: bool,
}

pub(super) fn execute(args: HostListArgs, global_root: &Path) -> CommandOut {
    run(Command::List {
        global_root: global_root.to_path_buf(),
        probe: !args.no_probe,
    })
}

/// A table of the hosts; `orbit host show` has each one's full detail.
pub(super) fn list_payload(list: HostList) -> CommandOut {
    let has_offline = list.hosts.iter().any(|host| host.offline.is_some());
    let mut columns = vec![
        Column::new("NAME"),
        Column::new("MACHINE_ID").fixed(),
        Column::new("SSH"),
        Column::new("PREFIX").fixed(),
        Column::new("REACHABLE").fixed(),
        Column::new("VERSION").fixed(),
        Column::new("PROTOCOL").path(),
        Column::new("WORKSPACES"),
        Column::new("ERROR"),
    ];
    if has_offline {
        columns.push(Column::new("OFFLINE REASON"));
    }
    let mut table = Table::new(columns);
    for host in &list.hosts {
        let workspaces = host
            .workspaces
            .iter()
            .map(|workspace| format!("{} ({})", workspace.name, workspace_role(workspace)))
            .collect::<Vec<_>>();
        let mut cells = vec![
            marked_name(host),
            host.machine_id.clone(),
            host.ssh.clone().unwrap_or_else(|| "-".to_string()),
            host.task_prefix
                .clone()
                .unwrap_or_else(|| UNKNOWN.to_string()),
            reachability(host).to_string(),
            host.binary_version
                .clone()
                .unwrap_or_else(|| UNKNOWN.to_string()),
            host.protocol_fingerprint
                .clone()
                .unwrap_or_else(|| UNKNOWN.to_string()),
            if workspaces.is_empty() {
                "-".to_string()
            } else {
                workspaces.join(", ")
            },
            host.error.as_ref().map_or_else(
                || "-".to_string(),
                |error| format!("{}: {}", error.code, error.message),
            ),
        ];
        if has_offline {
            cells.push(
                host.offline
                    .as_ref()
                    .map_or_else(|| "-".to_string(), |mark| mark.reason.clone()),
            );
        }
        table.add_row(cells);
    }
    let records = list
        .hosts
        .iter()
        .map(to_json)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Payload::enveloped_list(to_json(&list)?, records, table).into())
}
