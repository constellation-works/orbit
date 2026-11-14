//! `orbit host add|rename|remove` over the registered host entries.

use std::path::Path;

use orbit_common::{HostRegistryCode, OrbitError};
use orbit_registry::hosts::{HostEntry, ResolvedHost, load_host_registry, validate_ssh_target};
use orbit_types::identity::validate_machine_name;
use serde::Serialize;

use super::probe::{LiveHost, probe_ssh};
use super::{HostDependents, HostRow, host_dependents, remote_row, require_local};

/// What one `orbit host` mutation wrote.
#[derive(Debug, Clone, Serialize)]
pub struct HostChange {
    /// `added`, `renamed` or `removed`.
    pub action: &'static str,
    /// The entry as written, or as it was before removal.
    pub entry: HostChangeEntry,
    /// The previous name of a renamed entry.
    pub previous_name: Option<String>,
    /// Retained for JSON compatibility; always empty.
    pub migrated: Vec<HostEntry>,
    /// The live probe summary of an added host.
    pub host: Option<HostRow>,
    /// Dependents a forced removal left without a route.
    pub orphaned: Option<HostDependents>,
}

/// The cached identity reported by a host mutation.
#[derive(Debug, Clone, Serialize)]
pub struct HostChangeEntry {
    /// The registered name.
    pub name: String,
    /// The registered machine identity.
    pub machine_id: String,
    /// The registered SSH target.
    pub ssh: String,
    /// The registered task prefix, retained as optional for JSON compatibility.
    pub task_prefix: Option<String>,
}

impl From<HostEntry> for HostChangeEntry {
    fn from(entry: HostEntry) -> Self {
        Self {
            name: entry.name,
            machine_id: entry.machine_id,
            ssh: entry.ssh,
            task_prefix: Some(entry.task_prefix),
        }
    }
}

/// Probe `ssh`, validate the would-be entry, and register it. Writes nothing
/// on the remote and nothing here unless every check passes.
pub fn add_host(
    global_root: &Path,
    ssh: &str,
    name: Option<&str>,
) -> Result<HostChange, OrbitError> {
    validate_ssh_target(ssh)?;
    let registry = load_host_registry(global_root)?;
    let local = require_local(&registry)?;
    let live = probe_ssh(ssh, &local.id).map_err(|error| match error {
        OrbitError::UnreachableDestination(message) => OrbitError::UnreachableDestination(format!(
            "{message}; check that `ssh {ssh}` logs in without a prompt and that `orbit` is on \
             the remote PATH"
        )),
        other => other,
    })?;
    let facts = &live.facts;
    if facts.machine_id == local.id {
        return Err(OrbitError::host_registry(
            HostRegistryCode::HostIsLocal,
            format!(
                "'{ssh}' answered with this machine's own machine_id {}; the local host is \
                 always listed and is never added",
                local.id
            ),
        ));
    }
    if let Some(existing) = registry
        .hosts()
        .entries()
        .iter()
        .find(|entry| entry.machine_id == facts.machine_id)
    {
        return Err(OrbitError::host_registry(
            HostRegistryCode::HostExists,
            format!(
                "{} is already registered as '{}' (ssh {}); use `orbit host rename` to rename it",
                facts.machine_id, existing.name, existing.ssh
            ),
        ));
    }
    let Some(task_prefix) = facts.task_prefix.clone() else {
        return Err(too_old(ssh, &live));
    };
    if task_prefix == local.task_prefix {
        return Err(OrbitError::host_registry(
            HostRegistryCode::TaskPrefixConflict,
            format!(
                "'{ssh}' is {}, a different machine, but it mints task ids with this machine's \
                 own prefix {task_prefix}. A task id must name exactly one host, and a prefix is \
                 fixed when `orbit init` runs, so that host cannot be registered here",
                facts.machine_id
            ),
        ));
    }
    let name = match name {
        Some(name) => name.trim().to_string(),
        None => facts
            .machine_name
            .clone()
            .unwrap_or_else(|| ssh.to_string()),
    };
    validate_machine_name(&name)?;
    let entry = HostEntry {
        name,
        machine_id: facts.machine_id.clone(),
        ssh: ssh.to_string(),
        task_prefix,
        host_os: facts.host_os,
    };
    let mut entries = registry.hosts().entries().to_vec();
    entries.push(entry.clone());
    registry
        .commit(entries)
        .map_err(|error| name_hint(error, "choose another with --name"))?;
    let host = remote_row_from(&entry, live);
    Ok(HostChange {
        action: "added",
        entry: entry.into(),
        previous_name: None,
        migrated: Vec::new(),
        host: Some(host),
        orphaned: None,
    })
}

/// Rename a remote entry. The local host's name is `machine.name`.
pub fn rename_host(
    global_root: &Path,
    selector: &str,
    new_name: &str,
) -> Result<HostChange, OrbitError> {
    let registry = load_host_registry(global_root)?;
    require_local(&registry)?;
    let machine_id = match registry.resolve(selector)? {
        ResolvedHost::Local(_) => {
            return Err(OrbitError::host_registry(
                HostRegistryCode::HostIsLocal,
                "this machine is the local host; rename it with \
                 `orbit config set --global machine.name <value>`",
            ));
        }
        resolved => resolved.machine_id().to_string(),
    };
    let new_name = new_name.trim().to_string();
    validate_machine_name(&new_name)?;
    let mut entries = registry.hosts().entries().to_vec();
    let Some(entry) = entries
        .iter_mut()
        .find(|entry| entry.machine_id == machine_id)
    else {
        return Err(vanished(&machine_id));
    };
    let previous_name = std::mem::replace(&mut entry.name, new_name);
    let renamed = entry.clone();
    registry
        .commit(entries)
        .map_err(|error| name_hint(error, "choose another new name"))?;
    Ok(HostChange {
        action: "renamed",
        entry: renamed.into(),
        previous_name: Some(previous_name),
        migrated: Vec::new(),
        host: None,
        orphaned: None,
    })
}

/// Remove a remote entry. Refused while a local replica checkout or a pull
/// drain routes to the host, unless `force`.
pub fn remove_host(
    global_root: &Path,
    selector: &str,
    force: bool,
) -> Result<HostChange, OrbitError> {
    let registry = load_host_registry(global_root)?;
    require_local(&registry)?;
    let removed = match registry.resolve(selector)? {
        ResolvedHost::Local(_) => {
            return Err(OrbitError::host_registry(
                HostRegistryCode::HostIsLocal,
                "this machine is the local host, which is never an entry and cannot be removed",
            ));
        }
        ResolvedHost::Entry(entry) => HostChangeEntry::from(entry.clone()),
    };
    let machine_id = &removed.machine_id;
    let dependents = host_dependents(global_root, machine_id)?;
    if !dependents.is_empty() && !force {
        return Err(OrbitError::host_registry(
            HostRegistryCode::HostInUse,
            format!(
                "{machine_id} is still the owner route for: {}. Remove or re-home those first, or \
                 pass --force to remove the entry anyway",
                dependents.describe().join("; ")
            ),
        ));
    }
    let entries = registry
        .hosts()
        .entries()
        .iter()
        .filter(|entry| entry.machine_id != *machine_id)
        .cloned()
        .collect();
    registry.commit(entries)?;
    Ok(HostChange {
        action: "removed",
        entry: removed,
        previous_name: None,
        migrated: Vec::new(),
        host: None,
        orphaned: (!dependents.is_empty()).then_some(dependents),
    })
}

fn remote_row_from(entry: &HostEntry, live: LiveHost) -> HostRow {
    let mut row = remote_row(entry, "", false);
    row.reachable = Some(true);
    row.skew_fields = super::skew_fields(&live.facts);
    row.skew = !row.skew_fields.is_empty();
    row.binary_version = live.facts.binary_version;
    row.protocol_fingerprint = live.facts.protocol_fingerprint;
    row.workspaces = live
        .workspaces
        .iter()
        .map(|workspace| super::remote_workspace(workspace, &entry.machine_id))
        .collect();
    row
}

/// Finish a name conflict with the remedy the refused command actually takes.
fn name_hint(error: OrbitError, hint: &str) -> OrbitError {
    match error {
        OrbitError::HostRegistry {
            code: HostRegistryCode::HostNameConflict,
            message,
        } => OrbitError::host_registry(
            HostRegistryCode::HostNameConflict,
            format!("{message}; {hint}"),
        ),
        other => other,
    }
}

fn too_old(ssh: &str, live: &LiveHost) -> OrbitError {
    let message = match live.facts.binary_version.as_deref() {
        Some(version) => format!(
            "'{ssh}' runs Orbit {version} but reported no task prefix; run `orbit init` on it \
             so it has a [machine] identity, then rerun"
        ),
        None => format!(
            "'{ssh}' runs an Orbit that predates host identity (it reports no version or task \
             prefix); upgrade it to {} or later, then rerun",
            super::local_binary_version()
        ),
    };
    OrbitError::host_registry(HostRegistryCode::HostTooOld, message)
}

fn vanished(machine_id: &str) -> OrbitError {
    OrbitError::host_registry(
        HostRegistryCode::UnknownHost,
        format!("{machine_id} is no longer registered; run `orbit host list`"),
    )
}
