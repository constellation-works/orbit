//! `orbit host add|rename|remove`, and the one-time migration of the legacy
//! destinations file that the first of them performs.

use std::path::Path;

use orbit_common::{HostRegistryCode, OrbitError};
use orbit_registry::MachineIdentity;
use orbit_registry::hosts::{
    HostEntry, HostRegistry, LegacyHost, RegisteredHosts, ResolvedHost, legacy_destinations_path,
    load_host_registry, validate_ssh_target,
};
use orbit_types::identity::validate_machine_name;
use serde::Serialize;

use super::probe::{LiveHost, in_parallel, probe_ssh};
use super::{HostDependents, HostRow, RemoteTarget, host_dependents, remote_row, require_local};

/// What one `orbit host` mutation wrote.
#[derive(Debug, Clone, Serialize)]
pub struct HostChange {
    /// `added`, `migrated`, `renamed` or `removed`.
    pub action: &'static str,
    /// The entry as written, or as it was before removal.
    pub entry: HostChangeEntry,
    /// The previous name of a renamed entry.
    pub previous_name: Option<String>,
    /// Legacy rows this mutation migrated into the host file.
    pub migrated: Vec<HostEntry>,
    /// The live probe summary of an added host.
    pub host: Option<HostRow>,
    /// Dependents a forced removal left without a route.
    pub orphaned: Option<HostDependents>,
}

/// The cached identity reported by a host mutation.
#[derive(Debug, Clone, Serialize)]
pub struct HostChangeEntry {
    /// The registered name, or the SSH target of a removed legacy row.
    pub name: String,
    /// The registered machine identity.
    pub machine_id: String,
    /// The registered SSH target.
    pub ssh: String,
    /// Unknown when removing a legacy row without probing it.
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
    let migrated = migrated_entries(&registry, local, None)?;
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
    if let Some(entry) = migrated
        .iter()
        .find(|entry| entry.machine_id == facts.machine_id)
        .cloned()
    {
        registry.commit(migrated.clone())?;
        let host = remote_row_from(&entry, live);
        return Ok(HostChange {
            action: "migrated",
            entry: entry.into(),
            previous_name: None,
            migrated,
            host: Some(host),
            orphaned: None,
        });
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
    };
    let mut entries = registered(&registry, &migrated).to_vec();
    entries.push(entry.clone());
    registry
        .commit(entries)
        .map_err(|error| name_hint(error, "choose another with --name"))?;
    let host = remote_row_from(&entry, live);
    Ok(HostChange {
        action: "added",
        entry: entry.into(),
        previous_name: None,
        migrated: newly_migrated(&registry, migrated),
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
    let local = require_local(&registry)?;
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
    let migrated = migrated_entries(&registry, local, None)?;
    let mut entries = registered(&registry, &migrated).to_vec();
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
        migrated: newly_migrated(&registry, migrated),
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
    let local = require_local(&registry)?;
    let removed = match registry.resolve(selector)? {
        ResolvedHost::Local(_) => {
            return Err(OrbitError::host_registry(
                HostRegistryCode::HostIsLocal,
                "this machine is the local host, which is never an entry and cannot be removed",
            ));
        }
        ResolvedHost::Entry(entry) => HostChangeEntry::from(entry.clone()),
        ResolvedHost::Legacy(row) => HostChangeEntry {
            name: row.ssh.clone(),
            machine_id: row.machine_id.clone(),
            ssh: row.ssh.clone(),
            task_prefix: None,
        },
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
    let migrated = migrated_entries(&registry, local, Some(machine_id))?;
    let entries = registered(&registry, &migrated)
        .iter()
        .filter(|entry| entry.machine_id != *machine_id)
        .cloned()
        .collect();
    registry.commit(entries)?;
    Ok(HostChange {
        action: "removed",
        entry: removed,
        previous_name: None,
        migrated: newly_migrated(&registry, migrated),
        host: None,
        orphaned: (!dependents.is_empty()).then_some(dependents),
    })
}

/// The entries a mutation starts from: the host file's, or — on the first
/// mutation after the legacy file — every retained legacy row probed into an entry.
///
/// A retained legacy row that does not answer refuses the whole mutation and touches
/// neither file. Each migrated row is named after the remote's
/// `machine.name`, or after its SSH target when that name is taken. A row for
/// this machine was never a route and is dropped. Removal also excludes its
/// selected row, whose cached identity suffices to drop it.
fn migrated_entries(
    registry: &HostRegistry,
    local: &MachineIdentity,
    removing: Option<&str>,
) -> Result<Vec<HostEntry>, OrbitError> {
    let rows = match registry.hosts() {
        RegisteredHosts::None => return Ok(Vec::new()),
        RegisteredHosts::Hosts(entries) => return Ok(entries.clone()),
        RegisteredHosts::Legacy(rows) => rows
            .iter()
            .filter(|row| row.machine_id != local.id && Some(row.machine_id.as_str()) != removing)
            .collect::<Vec<_>>(),
    };
    let legacy_path = legacy_destinations_path(registry.global_root());
    let answers = in_parallel(&rows, |row| probe_ssh(&row.ssh, &local.id));
    let mut entries: Vec<HostEntry> = Vec::with_capacity(rows.len());
    for (row, answer) in rows.into_iter().zip(answers) {
        let live = answer.map_err(|error| legacy_unreachable(row, &legacy_path, &error))?;
        if live.facts.machine_id != row.machine_id {
            return Err(OrbitError::host_registry(
                HostRegistryCode::HostIdentityMismatch,
                format!(
                    "legacy row ssh {} in '{}' records {} but the host answered as {}; fix or \
                     delete that row, then rerun",
                    row.ssh,
                    legacy_path.display(),
                    row.machine_id,
                    live.facts.machine_id
                ),
            ));
        }
        let Some(task_prefix) = live.facts.task_prefix.clone() else {
            return Err(too_old(&row.ssh, &live));
        };
        let taken = |name: &str| {
            name.eq_ignore_ascii_case(&local.name)
                || entries
                    .iter()
                    .any(|entry| entry.name.eq_ignore_ascii_case(name))
        };
        let name = live
            .facts
            .machine_name
            .clone()
            .filter(|name| validate_machine_name(name).is_ok() && !taken(name))
            .unwrap_or_else(|| row.ssh.clone());
        entries.push(HostEntry {
            name,
            machine_id: row.machine_id.clone(),
            ssh: row.ssh.clone(),
            task_prefix,
        });
    }
    Ok(entries)
}

/// The host file's entries, or the migrated legacy rows standing in for them.
fn registered<'a>(registry: &'a HostRegistry, migrated: &'a [HostEntry]) -> &'a [HostEntry] {
    match registry.hosts() {
        RegisteredHosts::Hosts(entries) => entries,
        RegisteredHosts::None | RegisteredHosts::Legacy(_) => migrated,
    }
}

fn newly_migrated(registry: &HostRegistry, migrated: Vec<HostEntry>) -> Vec<HostEntry> {
    match registry.hosts() {
        RegisteredHosts::Legacy(_) => migrated,
        RegisteredHosts::None | RegisteredHosts::Hosts(_) => Vec::new(),
    }
}

fn remote_row_from(entry: &HostEntry, live: LiveHost) -> HostRow {
    let mut row = remote_row(&RemoteTarget::Entry(entry), "", false);
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

fn legacy_unreachable(row: &LegacyHost, legacy_path: &Path, error: &OrbitError) -> OrbitError {
    OrbitError::host_registry(
        HostRegistryCode::LegacyHostUnreachable,
        format!(
            "migrating '{}' needs every retained row to answer, and ssh {} ({}) did not: {error}. \
             Make it reachable, or remove it with `orbit host remove {}`, then rerun; neither \
             file was changed",
            legacy_path.display(),
            row.ssh,
            row.machine_id,
            row.machine_id
        ),
    )
}

fn vanished(machine_id: &str) -> OrbitError {
    OrbitError::host_registry(
        HostRegistryCode::UnknownHost,
        format!("{machine_id} is no longer registered; run `orbit host list`"),
    )
}
