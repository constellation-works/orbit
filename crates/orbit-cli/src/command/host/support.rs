use std::fmt::Write as _;

use orbit_cmd::hosts::{HostChange, HostRow, HostWorkspace};
use orbit_core::OrbitError;
use serde_json::Value;

use crate::command::{CommandOut, Payload};

pub(super) fn to_json(value: &impl serde::Serialize) -> Result<Value, OrbitError> {
    serde_json::to_value(value)
        .map_err(|error| OrbitError::Execution(format!("serialize host report: {error}")))
}

pub(super) fn change_payload(change: HostChange) -> CommandOut {
    let mut text = String::new();
    let entry = &change.entry;
    match change.action {
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
        "offline" => {
            let reason = entry
                .offline
                .as_ref()
                .map(|mark| mark.reason.as_str())
                .unwrap_or_default();
            let _ = writeln!(text, "{} acknowledged offline: {reason}", entry.name);
        }
        "online" => {
            let _ = writeln!(text, "cleared offline acknowledgement for {}", entry.name);
        }
        "already_online" => {
            let _ = writeln!(
                text,
                "offline acknowledgement for {} is already clear",
                entry.name
            );
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

pub(super) const UNKNOWN: &str = "unknown";

/// The host name with its `local` and `SKEW` marks.
pub(super) fn marked_name(host: &HostRow) -> String {
    let mut marks = Vec::new();
    if host.local {
        marks.push("local");
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
pub(super) fn reachability(host: &HostRow) -> &'static str {
    if host.offline.is_some() {
        return if host.reachable == Some(true) && host.error.is_none() {
            "acknowledged offline (responded)"
        } else {
            "acknowledged offline"
        };
    }
    match host.reachable {
        Some(true) => "yes",
        Some(false) => "no",
        None => "not probed",
    }
}

/// Render the shared acknowledgement rows from an observational payload.
pub(crate) fn acknowledged_host_lines(payload: &Value) -> impl Iterator<Item = String> {
    payload["acknowledged"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|host| {
            let name = host["name"].as_str()?;
            let reason = host["reason"].as_str()?;
            Some(format!("{name} acknowledged offline: {reason}"))
        })
}

pub(super) fn workspace_role(workspace: &HostWorkspace) -> String {
    match (workspace.role, workspace.owner_machine_id.as_deref()) {
        ("replica", Some(owner)) => format!("replica of {owner}"),
        (role, _) => role.to_string(),
    }
}

pub(super) fn host_block(host: &HostRow) -> String {
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
    if let Some(mark) = &host.offline {
        let _ = writeln!(text, "  reason:      {}", mark.reason);
        let _ = writeln!(
            text,
            "  since:       {} by {}",
            mark.since.to_rfc3339(),
            mark.set_by
        );
        if let Some(until) = mark.until {
            let _ = writeln!(text, "  until:       {}", until.to_rfc3339());
        }
    }
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

pub(super) fn skew_note(host: &HostRow, field: &str) -> &'static str {
    if host.skew_fields.contains(&field) {
        "  (differs from this machine)"
    } else {
        ""
    }
}
