//! `orbit host` — the remote Orbit hosts this machine reaches over SSH.
//!
//! Each command loads `~/.orbit/hosts.toml` through `orbit_cmd::hosts`, which
//! the dashboard also calls, and opens no workspace runtime. Identity, version
//! and protocol are read from each host live; only its name, SSH target,
//! `machine_id` and task prefix are stored.

use std::fmt::Write as _;
use std::path::Path;

use clap::{Args, Subcommand};
use orbit_cmd::hosts::{self, HostChange, HostDetail, HostList, HostRow, HostWorkspace};
use orbit_core::OrbitError;
use serde_json::Value;

use crate::command::{CommandOut, Payload};
use crate::output::table::{Column, Table};

#[derive(Args)]
#[command(
    about = "Register and inspect the remote Orbit hosts this machine reaches over SSH",
    long_about = "Register and inspect the remote Orbit hosts this machine reaches over SSH\n\n\
        Federated MCP, pull drains and replica worktree GC route to the hosts registered here. \
        Each entry stores the host's name, SSH target, machine_id and task prefix, read from the \
        host itself when it is added. Reachability, version, pull protocol and workspaces are \
        read live from each host whenever a command asks."
)]
pub struct HostCommand {
    #[command(subcommand)]
    pub command: HostSubcommand,
}

#[derive(Subcommand)]
pub enum HostSubcommand {
    /// Register a remote host, reading its identity from the host over SSH
    Add(HostAddArgs),
    /// List this host and every registered host with live reachability, version and protocol
    List(HostListArgs),
    /// Show one host and what on this machine routes to it
    Show(HostShowArgs),
    /// Rename a registered host
    Rename(HostRenameArgs),
    /// Remove a registered host
    Remove(HostRemoveArgs),
}

#[derive(Args)]
#[command(
    after_help = "Examples:\n  orbit host add dk-server-2\n  orbit host add daniel@10.0.0.7 --name build-box\n\n\
        With only mcp-destinations.toml, this migrates every legacy host to hosts.toml. \
        Adding a host already in the legacy file succeeds and reports the migration, preserving \
        its SSH target and migrated name (--name applies only to a new host). Every retained \
        legacy host must answer. A host already in hosts.toml still refuses with host_exists."
)]
pub struct HostAddArgs {
    /// SSH alias or user@host that reaches the remote host
    #[arg(value_name = "SSH_TARGET")]
    pub ssh_target: String,
    /// Name for the host (defaults to the remote's own machine.name)
    #[arg(long)]
    pub name: Option<String>,
    /// Emit machine-readable JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct HostListArgs {
    /// Print the stored fields only, without contacting any host
    #[arg(long)]
    pub no_probe: bool,
    /// Emit machine-readable JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct HostShowArgs {
    /// Host name (case-insensitive) or machine_id
    #[arg(value_name = "HOST")]
    pub host: String,
    /// Emit machine-readable JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct HostRenameArgs {
    /// Host name (case-insensitive) or machine_id
    #[arg(value_name = "HOST")]
    pub host: String,
    /// The new name
    #[arg(value_name = "NEW_NAME")]
    pub new_name: String,
    /// Emit machine-readable JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
#[command(
    after_help = "With only mcp-destinations.toml, removal migrates the retained rows to hosts.toml. \
        The removed legacy host is never contacted, even without --force; every retained host \
        must answer. --force only overrides dependent replica checkouts and pull drains."
)]
pub struct HostRemoveArgs {
    /// Host name (case-insensitive) or machine_id
    #[arg(value_name = "HOST")]
    pub host: String,
    /// Remove the entry even while a replica checkout or pull drain routes to it
    #[arg(long)]
    pub force: bool,
    /// Emit machine-readable JSON
    #[arg(long)]
    pub json: bool,
}

impl HostCommand {
    pub fn execute_without_runtime(self, root_override: Option<&Path>) -> CommandOut {
        let global_root = orbit_core::runtime::resolve_generation_root(root_override)?;
        match self.command {
            HostSubcommand::Add(args) => change_payload(hosts::add_host(
                &global_root,
                &args.ssh_target,
                args.name.as_deref(),
            )?),
            HostSubcommand::List(args) => {
                list_payload(hosts::list_hosts(&global_root, !args.no_probe)?)
            }
            HostSubcommand::Show(args) => show_payload(hosts::show_host(&global_root, &args.host)?),
            HostSubcommand::Rename(args) => change_payload(hosts::rename_host(
                &global_root,
                &args.host,
                &args.new_name,
            )?),
            HostSubcommand::Remove(args) => {
                change_payload(hosts::remove_host(&global_root, &args.host, args.force)?)
            }
        }
    }
}

fn to_json(value: &impl serde::Serialize) -> Result<Value, OrbitError> {
    serde_json::to_value(value)
        .map_err(|error| OrbitError::Execution(format!("serialize host report: {error}")))
}

/// A table of the hosts; `orbit host show` has each one's full detail.
fn list_payload(list: HostList) -> CommandOut {
    let mut table = Table::new(vec![
        Column::new("NAME"),
        Column::new("MACHINE_ID").fixed(),
        Column::new("SSH"),
        Column::new("PREFIX").fixed(),
        Column::new("REACHABLE").fixed(),
        Column::new("VERSION").fixed(),
        Column::new("PROTOCOL").path(),
        Column::new("WORKSPACES"),
        Column::new("ERROR"),
    ]);
    for host in &list.hosts {
        let workspaces = host
            .workspaces
            .iter()
            .map(|workspace| format!("{} ({})", workspace.name, workspace_role(workspace)))
            .collect::<Vec<_>>();
        table.add_row(vec![
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
        ]);
    }
    if list.legacy {
        table = table.trailing_notice(format!(
            "Read from the legacy mcp-destinations.toml; the next `orbit host add`, `rename` or \
             `remove` migrates it to {}.",
            list.host_file.display()
        ));
    }
    let records = list
        .hosts
        .iter()
        .map(to_json)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Payload::enveloped_list(to_json(&list)?, records, table).into())
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

fn change_payload(change: HostChange) -> CommandOut {
    let mut text = String::new();
    for migrated in &change.migrated {
        let _ = writeln!(
            text,
            "migrated {} ({}, ssh {}, prefix {}) from mcp-destinations.toml",
            migrated.name, migrated.machine_id, migrated.ssh, migrated.task_prefix
        );
    }
    let entry = &change.entry;
    match change.action {
        "migrated" => {
            if let Some(host) = &change.host {
                text.push_str(&host_block(host));
            }
        }
        "renamed" => {
            let _ = writeln!(
                text,
                "renamed {} to {} ({})",
                change.previous_name.as_deref().unwrap_or("?"),
                entry.name,
                entry.machine_id
            );
        }
        "removed" => {
            let _ = writeln!(text, "removed {} ({})", entry.name, entry.machine_id);
            if let Some(orphaned) = &change.orphaned {
                text.push_str("these lose their owner route:\n");
                for line in orphaned.describe() {
                    let _ = writeln!(text, "  {line}");
                }
            }
        }
        _ => {
            let _ = writeln!(text, "added {} ({})", entry.name, entry.machine_id);
            if let Some(host) = &change.host {
                text.push_str(&host_block(host));
            }
        }
    }
    Ok(Payload::detail(to_json(&change)?, text).into())
}

const UNKNOWN: &str = "unknown";

/// The host name with its `local`, `legacy` and `SKEW` marks.
fn marked_name(host: &HostRow) -> String {
    let mut marks = Vec::new();
    if host.local {
        marks.push("local");
    }
    if host.legacy {
        marks.push("legacy");
    }
    if host.skew {
        marks.push("SKEW");
    }
    if marks.is_empty() {
        host.name.clone()
    } else {
        format!("{} [{}]", host.name, marks.join(", "))
    }
}

/// `reachable` exactly as the JSON reports it. A host that answered with the
/// wrong identity is reachable; its error is shown beside it, not folded in.
fn reachability(host: &HostRow) -> &'static str {
    match host.reachable {
        Some(true) => "yes",
        Some(false) => "no",
        None => "not probed",
    }
}

fn workspace_role(workspace: &HostWorkspace) -> String {
    match (workspace.role, workspace.owner_machine_id.as_deref()) {
        ("replica", Some(owner)) => format!("replica of {owner}"),
        (role, _) => role.to_string(),
    }
}

fn host_block(host: &HostRow) -> String {
    let mut text = format!("{}\n", marked_name(host));
    let _ = writeln!(text, "  machine_id:  {}", host.machine_id);
    let _ = writeln!(
        text,
        "  ssh:         {}",
        host.ssh.as_deref().unwrap_or("-")
    );
    let _ = writeln!(
        text,
        "  task_prefix: {}",
        host.task_prefix.as_deref().unwrap_or(UNKNOWN)
    );
    let _ = writeln!(text, "  reachable:   {}", reachability(host));
    if let Some(error) = &host.error {
        let _ = writeln!(text, "  error:       {}: {}", error.code, error.message);
    }
    let _ = writeln!(
        text,
        "  version:     {}{}",
        host.binary_version.as_deref().unwrap_or(UNKNOWN),
        skew_note(host, "binary_version")
    );
    let _ = writeln!(
        text,
        "  protocol:    {}{}",
        host.protocol_fingerprint.as_deref().unwrap_or(UNKNOWN),
        skew_note(host, "protocol_fingerprint")
    );
    if host.workspaces.is_empty() {
        let _ = writeln!(text, "  workspaces:  -");
    } else {
        let _ = writeln!(text, "  workspaces:");
        for workspace in &host.workspaces {
            let _ = writeln!(
                text,
                "    {} ({}) {}",
                workspace.name,
                workspace.id,
                workspace_role(workspace)
            );
        }
    }
    text
}

fn skew_note(host: &HostRow, field: &str) -> &'static str {
    if host.skew_fields.contains(&field) {
        "  (differs from this machine)"
    } else {
        ""
    }
}
