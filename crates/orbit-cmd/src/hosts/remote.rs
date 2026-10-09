//! Remote host rows: a registered host's cached identity, joined with what a
//! live probe reports when asked. A probe answering as another machine fails
//! closed and is never attributed to the entry.

use orbit_common::{HostRegistryCode, OrbitError};
use orbit_registry::hosts::{HostEntry, LegacyHost, RegisteredHosts};
use orbit_types::workspace::Workspace;

use super::probe::{
    self, LiveHost, error_class, error_detail, local_binary_version, local_protocol_fingerprint,
};
use super::{HostProbeError, HostRow, HostWorkspace};

/// A remote row's cached identity: a host-file entry or a legacy route.
#[derive(Debug, Clone, Copy)]
pub(super) enum RemoteTarget<'a> {
    Entry(&'a HostEntry),
    Legacy(&'a LegacyHost),
}

impl RemoteTarget<'_> {
    pub(super) fn name(&self) -> &str {
        match self {
            Self::Entry(entry) => &entry.name,
            Self::Legacy(row) => &row.ssh,
        }
    }

    pub(super) fn ssh(&self) -> &str {
        match self {
            Self::Entry(entry) => &entry.ssh,
            Self::Legacy(row) => &row.ssh,
        }
    }

    pub(super) fn machine_id(&self) -> &str {
        match self {
            Self::Entry(entry) => &entry.machine_id,
            Self::Legacy(row) => &row.machine_id,
        }
    }

    pub(super) fn task_prefix(&self) -> Option<&str> {
        match self {
            Self::Entry(entry) => Some(&entry.task_prefix),
            Self::Legacy(_) => None,
        }
    }
}

pub(super) fn remote_targets(hosts: &RegisteredHosts) -> Vec<RemoteTarget<'_>> {
    hosts
        .entries()
        .iter()
        .map(RemoteTarget::Entry)
        .chain(hosts.legacy().iter().map(RemoteTarget::Legacy))
        .collect()
}

pub(super) fn remote_row(
    target: &RemoteTarget<'_>,
    caller_machine_id: &str,
    probe: bool,
) -> HostRow {
    let mut row = HostRow {
        name: target.name().to_string(),
        machine_id: target.machine_id().to_string(),
        ssh: Some(target.ssh().to_string()),
        task_prefix: target.task_prefix().map(ToOwned::to_owned),
        local: false,
        legacy: matches!(target, RemoteTarget::Legacy(_)),
        reachable: None,
        error: None,
        binary_version: None,
        protocol_fingerprint: None,
        skew: false,
        skew_fields: Vec::new(),
        workspaces: Vec::new(),
    };
    if !probe {
        return row;
    }
    let live = probe::probe_ssh(target.ssh(), caller_machine_id)
        .and_then(|live| verify_identity(target, live));
    match live {
        Ok(live) => {
            row.reachable = Some(true);
            if row.task_prefix.is_none() {
                row.task_prefix = live.facts.task_prefix.clone();
            }
            row.skew_fields = skew_fields(&live.facts);
            row.skew = !row.skew_fields.is_empty();
            row.binary_version = live.facts.binary_version;
            row.protocol_fingerprint = live.facts.protocol_fingerprint;
            row.workspaces = live
                .workspaces
                .iter()
                .map(|workspace| remote_workspace(workspace, target.machine_id()))
                .collect();
        }
        Err(error) => {
            // An identity mismatch did answer; it is listed as reachable, but
            // none of what it said is attributed to this entry.
            row.reachable =
                Some(error.host_registry_code() == Some(HostRegistryCode::HostIdentityMismatch));
            row.error = Some(HostProbeError {
                code: error_class(&error),
                message: error_detail(&error),
            });
        }
    }
    row
}

/// A probe answering with another identity fails closed: the entry is never
/// rewritten from a probe, and a replaced host is removed and added again.
fn verify_identity(target: &RemoteTarget<'_>, live: LiveHost) -> Result<LiveHost, OrbitError> {
    let reported_prefix = live.facts.task_prefix.as_deref();
    let prefix_differs = matches!(
        (target.task_prefix(), reported_prefix),
        (Some(cached), Some(reported)) if cached != reported
    );
    if live.facts.machine_id == target.machine_id() && !prefix_differs {
        return Ok(live);
    }
    Err(OrbitError::host_registry(
        HostRegistryCode::HostIdentityMismatch,
        format!(
            "'{}' answered as machine_id {} with task prefix {}, but the entry records {} with \
             task prefix {}; the entry is unchanged. If the host was reinstalled or replaced, run \
             `orbit host remove {}` and then `orbit host add {}`",
            target.ssh(),
            live.facts.machine_id,
            reported_prefix.unwrap_or("unknown"),
            target.machine_id(),
            target.task_prefix().unwrap_or("unknown"),
            target.name(),
            target.ssh()
        ),
    ))
}

pub(super) fn skew_fields(facts: &orbit_mcp::HostFacts) -> Vec<&'static str> {
    let mut fields = Vec::new();
    if facts
        .binary_version
        .as_deref()
        .is_some_and(|version| version != local_binary_version())
    {
        fields.push("binary_version");
    }
    if facts
        .protocol_fingerprint
        .as_deref()
        .is_some_and(|fingerprint| fingerprint != local_protocol_fingerprint())
    {
        fields.push("protocol_fingerprint");
    }
    fields
}

/// A remote workspace's role on that host: it owns what its record says it
/// owns (or a pre-identity standalone record), and replicates everything else.
pub(super) fn remote_workspace(workspace: &Workspace, host_machine_id: &str) -> HostWorkspace {
    let replica = workspace
        .owner_machine_id
        .as_deref()
        .is_some_and(|owner| owner != host_machine_id);
    HostWorkspace {
        id: workspace.id.clone(),
        name: workspace.name.clone(),
        role: if replica { "replica" } else { "owner" },
        owner_machine_id: workspace.owner_machine_id.clone(),
        status: workspace.status.to_string(),
    }
}
