//! The operator's registered remote hosts [ORB-14448].
//!
//! `~/.orbit/hosts.toml` is the only record of remote membership. Federated
//! serve, pull drains and replica worktree GC read it through
//! [`load_host_routes`]; `orbit host` writes it through [`HostRegistry::commit`].
//! An entry stores the operator's name for a host, its SSH target, and the two
//! facts that never change on that host: `machine_id` and `task_prefix`.
//! Anything that can change (reachability, version, protocol, workspaces) is
//! read live from the host and never persisted here.
//!
//! The same file is the task-id routing table: [`TaskPrefixTable`] maps this
//! machine's `machine.task_prefix` and each entry's `task_prefix` to the host
//! that writes those ids, so a call addressing one task by id goes to the host
//! its prefix names and nowhere else [ORB-14449].
//!
//! One release of compatibility: when only the legacy
//! `~/.orbit/mcp-destinations.toml` exists, its rows are read as routes with
//! no name or prefix. The first `orbit host` mutation migrates them and
//! deletes the legacy file. Both files at once is refused on every load,
//! because they are two answers to one question.

use std::collections::HashSet;
use std::io;
use std::path::{Component, Path, PathBuf};

use orbit_common::fs::io::{atomic_write_text, with_exclusive_file_lock};
use orbit_common::{HostRegistryCode, OrbitError};
use orbit_types::identity::{
    validate_machine_id, validate_machine_name, validate_stored_task_prefix,
};
use orbit_types::task::task_id_prefix;
use serde::{Deserialize, Serialize};

use crate::machine_identity::MachineIdentity;
use crate::{MachineIdentityState, inspect_machine_identity};

/// The machine-global host file, beside `workspaces.json`.
pub const HOSTS_FILE: &str = "hosts.toml";
/// The hand-edited predecessor of [`HOSTS_FILE`], read for one release.
pub const LEGACY_DESTINATIONS_FILE: &str = "mcp-destinations.toml";
/// The host file schema this build reads and writes.
pub const HOSTS_SCHEMA_VERSION: u32 = 1;

/// One registered remote host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostEntry {
    /// The operator's name for the host; what people type.
    pub name: String,
    /// The host's own `[machine] id`, read from it at registration.
    pub machine_id: String,
    /// SSH alias or `user@host` the session opens.
    pub ssh: String,
    /// The host's own `machine.task_prefix`, read from it at registration.
    pub task_prefix: String,
}

/// One row of the legacy destinations file: a route with no name or prefix.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyHost {
    pub ssh: String,
    pub machine_id: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HostFileDocument {
    schema_version: u32,
    #[serde(default)]
    hosts: Vec<HostEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyDocument {
    #[serde(default)]
    destinations: Vec<LegacyHost>,
}

/// The remote host set as it was loaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegisteredHosts {
    /// Neither file exists: this machine has no remote hosts.
    None,
    /// The host file.
    Hosts(Vec<HostEntry>),
    /// Only the legacy destinations file exists.
    Legacy(Vec<LegacyHost>),
}

impl RegisteredHosts {
    /// Every SSH route, in file order.
    pub fn routes(&self) -> Vec<HostRoute> {
        match self {
            Self::None => Vec::new(),
            Self::Hosts(entries) => entries
                .iter()
                .map(|entry| HostRoute {
                    ssh: entry.ssh.clone(),
                    machine_id: entry.machine_id.clone(),
                })
                .collect(),
            Self::Legacy(rows) => rows
                .iter()
                .map(|row| HostRoute {
                    ssh: row.ssh.clone(),
                    machine_id: row.machine_id.clone(),
                })
                .collect(),
        }
    }

    /// The registered entries; empty while only the legacy file exists.
    pub fn entries(&self) -> &[HostEntry] {
        match self {
            Self::Hosts(entries) => entries,
            Self::None | Self::Legacy(_) => &[],
        }
    }

    /// The legacy rows; empty unless only the legacy file exists.
    pub fn legacy(&self) -> &[LegacyHost] {
        match self {
            Self::Legacy(rows) => rows,
            Self::None | Self::Hosts(_) => &[],
        }
    }
}

/// An SSH route a consumer may open to one remote host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostRoute {
    pub ssh: String,
    pub machine_id: String,
}

/// A host a `<host>` argument resolved to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedHost<'a> {
    Local(&'a MachineIdentity),
    Entry(&'a HostEntry),
    Legacy(&'a LegacyHost),
}

impl ResolvedHost<'_> {
    pub fn machine_id(&self) -> &str {
        match self {
            Self::Local(identity) => &identity.id,
            Self::Entry(entry) => &entry.machine_id,
            Self::Legacy(row) => &row.machine_id,
        }
    }
}

/// Where one task id routes [ORB-14449].
///
/// A task id's prefix names its only writer, so the prefix alone picks the
/// host. Nothing here probes or searches: the table is the host file plus this
/// machine's own `machine.task_prefix`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskPrefixRoute {
    /// This host's prefix, an id with no parseable prefix, or any id before
    /// this machine has a `[machine]` identity: run in-process, as before.
    Local,
    /// A registered host's prefix.
    Host(HostEntry),
    /// A prefix neither this host nor any entry claims.
    Unregistered { prefix: String },
}

/// The prefix table one process builds: the local `machine.task_prefix` and
/// each host-file entry's `task_prefix`. Legacy destination rows carry no
/// prefix and contribute nothing. Host-file validation keeps prefixes unique,
/// so a lookup is exact.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskPrefixTable {
    local: Option<String>,
    hosts: Vec<HostEntry>,
}

impl TaskPrefixTable {
    pub fn new(local_prefix: Option<String>, hosts: Vec<HostEntry>) -> Self {
        Self {
            local: local_prefix,
            hosts,
        }
    }

    /// This machine's own prefix, absent before `orbit init`.
    pub fn local_prefix(&self) -> Option<&str> {
        self.local.as_deref()
    }

    /// The registered host that holds `prefix`, if any.
    pub fn host_for_prefix(&self, prefix: &str) -> Option<&HostEntry> {
        self.hosts.iter().find(|entry| entry.task_prefix == prefix)
    }

    /// Where `task_id` routes.
    pub fn route(&self, task_id: &str) -> TaskPrefixRoute {
        let Some(prefix) = task_id_prefix(task_id.trim()) else {
            return TaskPrefixRoute::Local;
        };
        if self.local.as_deref() == Some(prefix) {
            return TaskPrefixRoute::Local;
        }
        if let Some(entry) = self.host_for_prefix(prefix) {
            return TaskPrefixRoute::Host(entry.clone());
        }
        if self.local.is_none() {
            return TaskPrefixRoute::Local;
        }
        TaskPrefixRoute::Unregistered {
            prefix: prefix.to_string(),
        }
    }
}

/// The refusal for an id whose prefix no host claims. Routing never searches
/// hosts, so the caller registers the host or names a workspace explicitly.
pub fn unknown_task_prefix(task_id: &str, prefix: &str) -> OrbitError {
    OrbitError::host_registry(
        HostRegistryCode::UnknownTaskPrefix,
        format!(
            "task {task_id} has prefix '{prefix}', which is neither this host's nor a registered \
             host's. Run `orbit host list` to see the registered hosts, or `orbit host add \
             <ssh-target>` to register the host that minted it; to read a local mirror, pass \
             --workspace explicitly"
        ),
    )
}

/// The bytes both files held when they were loaded. A commit compares them
/// again under the lock, so a concurrent edit is refused rather than lost.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FileSnapshot {
    hosts: Option<Vec<u8>>,
    legacy: Option<Vec<u8>>,
}

impl FileSnapshot {
    fn read(global_root: &Path) -> Result<Self, OrbitError> {
        Ok(Self {
            hosts: read_optional(&hosts_path(global_root))?,
            legacy: read_optional(&legacy_destinations_path(global_root))?,
        })
    }
}

/// The loaded host file with this machine's identity, for reads and commits.
#[derive(Debug, Clone)]
pub struct HostRegistry {
    global_root: PathBuf,
    local: Option<MachineIdentity>,
    hosts: RegisteredHosts,
    snapshot: FileSnapshot,
}

/// `global_root` unchanged when it is absolute and has no `.` or `..`
/// component, so the fixed host-file names joined to it stay inside the
/// selected root. A caller holding a root it did not resolve itself, such as
/// a served dashboard, passes it through here before deriving any host path.
pub fn validated_host_root(global_root: &Path) -> Result<PathBuf, OrbitError> {
    let plain = global_root.is_absolute()
        && global_root.components().all(|component| {
            matches!(
                component,
                Component::Prefix(_) | Component::RootDir | Component::Normal(_)
            )
        });
    if !plain {
        return Err(OrbitError::InvalidInput(format!(
            "Orbit root '{}' must be an absolute path without '.' or '..' components",
            global_root.display()
        )));
    }
    Ok(global_root.to_path_buf())
}

pub fn hosts_path(global_root: &Path) -> PathBuf {
    global_root.join(HOSTS_FILE)
}

pub fn legacy_destinations_path(global_root: &Path) -> PathBuf {
    global_root.join(LEGACY_DESTINATIONS_FILE)
}

/// Load and validate the host file, or the legacy file when it is the only one.
///
/// Missing files are a valid local-only configuration. Invalid content, an
/// unsupported schema or both files at once fail closed and leave the bytes
/// untouched.
pub fn load_host_registry(global_root: &Path) -> Result<HostRegistry, OrbitError> {
    let local = match inspect_machine_identity(global_root)? {
        MachineIdentityState::Present(identity) => Some(identity),
        MachineIdentityState::Absent => None,
    };
    let snapshot = FileSnapshot::read(global_root)?;
    let hosts = parse_hosts(global_root, &snapshot, local.as_ref())?;
    Ok(HostRegistry {
        global_root: global_root.to_path_buf(),
        local,
        hosts,
        snapshot,
    })
}

/// The SSH routes every multi-host consumer opens: federated serve, pull
/// drains and replica worktree GC.
pub fn load_host_routes(global_root: &Path) -> Result<Vec<HostRoute>, OrbitError> {
    Ok(load_host_registry(global_root)?.hosts.routes())
}

/// The prefix table from the host file and this machine's identity.
pub fn load_task_prefix_table(global_root: &Path) -> Result<TaskPrefixTable, OrbitError> {
    Ok(load_host_registry(global_root)?.task_prefix_table())
}

impl HostRegistry {
    /// The prefix table task-id routing reads [ORB-14449].
    pub fn task_prefix_table(&self) -> TaskPrefixTable {
        TaskPrefixTable::new(
            self.local.as_ref().map(|local| local.task_prefix.clone()),
            self.hosts.entries().to_vec(),
        )
    }

    /// This machine's identity, absent before `orbit init`.
    pub fn local(&self) -> Option<&MachineIdentity> {
        self.local.as_ref()
    }

    pub fn hosts(&self) -> &RegisteredHosts {
        &self.hosts
    }

    pub fn global_root(&self) -> &Path {
        &self.global_root
    }

    /// Resolve `<host>` by exact host name (case-insensitive) or exact
    /// `machine_id`. A legacy row has no name, so its SSH target stands in.
    pub fn resolve(&self, selector: &str) -> Result<ResolvedHost<'_>, OrbitError> {
        let selector = selector.trim();
        if let Some(local) = &self.local
            && (local.id == selector || local.name.eq_ignore_ascii_case(selector))
        {
            return Ok(ResolvedHost::Local(local));
        }
        if let Some(entry) =
            self.hosts.entries().iter().find(|entry| {
                entry.machine_id == selector || entry.name.eq_ignore_ascii_case(selector)
            })
        {
            return Ok(ResolvedHost::Entry(entry));
        }
        if let Some(row) = self
            .hosts
            .legacy()
            .iter()
            .find(|row| row.machine_id == selector || row.ssh == selector)
        {
            return Ok(ResolvedHost::Legacy(row));
        }
        let mut known = self
            .local
            .iter()
            .map(|local| local.name.clone())
            .chain(self.hosts.entries().iter().map(|entry| entry.name.clone()))
            .chain(self.hosts.legacy().iter().map(|row| row.ssh.clone()))
            .collect::<Vec<_>>();
        known.sort();
        Err(OrbitError::host_registry(
            HostRegistryCode::UnknownHost,
            format!(
                "no host is named '{selector}' or has that machine_id; known hosts: {}. \
                 Run `orbit host list` to see them",
                if known.is_empty() {
                    "none".to_string()
                } else {
                    known.join(", ")
                }
            ),
        ))
    }

    /// Check would-be entries against every host-file invariant, including
    /// uniqueness against this machine's own name, id and prefix.
    pub fn validate_entries(&self, entries: &[HostEntry]) -> Result<(), OrbitError> {
        validate_entries(entries, self.local.as_ref())
    }

    /// Replace the host file with `entries`, atomically, and retire the
    /// legacy file if that is what was loaded.
    ///
    /// The entries are validated first and serialized canonically, sorted by
    /// name. Under the host-file lock both files are re-read; if either
    /// changed since [`load_host_registry`], the commit is refused and nothing
    /// is written.
    pub fn commit(&self, mut entries: Vec<HostEntry>) -> Result<(), OrbitError> {
        self.validate_entries(&entries)?;
        entries.sort_by(|left, right| {
            left.name
                .to_ascii_lowercase()
                .cmp(&right.name.to_ascii_lowercase())
                .then_with(|| left.name.cmp(&right.name))
        });
        let document = HostFileDocument {
            schema_version: HOSTS_SCHEMA_VERSION,
            hosts: entries,
        };
        let text = toml::to_string(&document)
            .map_err(|error| OrbitError::Execution(format!("serialize the host file: {error}")))?;
        let path = hosts_path(&self.global_root);
        with_exclusive_file_lock(&path, "host registry", || {
            if FileSnapshot::read(&self.global_root)? != self.snapshot {
                return Err(OrbitError::InvalidInput(format!(
                    "'{}' or '{}' changed while this command ran; nothing was written. Rerun it",
                    path.display(),
                    legacy_destinations_path(&self.global_root).display()
                )));
            }
            atomic_write_text(&path, &text)
                .map_err(|error| OrbitError::from_write_io(&path, error))?;
            if self.snapshot.legacy.is_some() {
                remove_legacy_file(&self.global_root)?;
            }
            Ok(())
        })
    }
}

/// Validate one SSH target: an SSH alias or `user@host`. Refuses a leading
/// `-` (an ssh option), whitespace and shell metacharacters, so the value can
/// only ever be a destination argument.
pub fn validate_ssh_target(ssh: &str) -> Result<(), OrbitError> {
    if ssh.is_empty() {
        return Err(OrbitError::InvalidInput(
            "ssh target must not be empty".to_string(),
        ));
    }
    if ssh.starts_with('-') {
        return Err(OrbitError::InvalidInput(format!(
            "ssh target '{ssh}' must not start with '-'"
        )));
    }
    if !ssh
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"._-@:[]".contains(&byte))
    {
        return Err(OrbitError::InvalidInput(format!(
            "ssh target '{ssh}' must be an SSH alias or user@host: ASCII letters, digits and \
             . _ - @ : [ ] only"
        )));
    }
    Ok(())
}

fn parse_hosts(
    global_root: &Path,
    snapshot: &FileSnapshot,
    local: Option<&MachineIdentity>,
) -> Result<RegisteredHosts, OrbitError> {
    let path = hosts_path(global_root);
    let legacy_path = legacy_destinations_path(global_root);
    match (&snapshot.hosts, &snapshot.legacy) {
        (Some(_), Some(_)) => Err(OrbitError::host_registry(
            HostRegistryCode::HostFileConflict,
            format!(
                "both '{}' and the legacy '{}' exist, and Orbit will not choose between them. \
                 Make sure every legacy row is in the host file (`orbit host list`), then \
                 delete the legacy file",
                path.display(),
                legacy_path.display()
            ),
        )),
        (Some(bytes), None) => {
            let document: HostFileDocument =
                toml::from_str(&utf8(bytes, &path)?).map_err(|error| invalid_file(&path, error))?;
            if document.schema_version != HOSTS_SCHEMA_VERSION {
                return Err(OrbitError::InvalidInput(format!(
                    "'{}' has schema_version {}; this Orbit reads schema_version \
                     {HOSTS_SCHEMA_VERSION}. Upgrade Orbit or restore a version-{HOSTS_SCHEMA_VERSION} \
                     file",
                    path.display(),
                    document.schema_version
                )));
            }
            validate_entries(&document.hosts, local).map_err(|error| in_file(error, &path))?;
            Ok(RegisteredHosts::Hosts(document.hosts))
        }
        (None, Some(bytes)) => {
            let document: LegacyDocument = toml::from_str(&utf8(bytes, &legacy_path)?)
                .map_err(|error| invalid_file(&legacy_path, error))?;
            validate_legacy(&document.destinations, &legacy_path)?;
            Ok(RegisteredHosts::Legacy(document.destinations))
        }
        (None, None) => Ok(RegisteredHosts::None),
    }
}

fn validate_entries(
    entries: &[HostEntry],
    local: Option<&MachineIdentity>,
) -> Result<(), OrbitError> {
    let mut names = HashSet::new();
    let mut machine_ids = HashSet::new();
    let mut prefixes = HashSet::new();
    for entry in entries {
        validate_machine_name(&entry.name)?;
        validate_machine_id(&entry.machine_id)?;
        validate_stored_task_prefix(&entry.task_prefix)?;
        validate_ssh_target(&entry.ssh)?;
        if let Some(local) = local {
            if entry.machine_id == local.id {
                return Err(OrbitError::host_registry(
                    HostRegistryCode::HostIsLocal,
                    format!(
                        "entry '{}' names this machine ({}); the local host is never an entry",
                        entry.name, local.id
                    ),
                ));
            }
            if entry.name.eq_ignore_ascii_case(&local.name) {
                return Err(name_conflict(&entry.name, "this machine's machine.name"));
            }
            if entry.task_prefix == local.task_prefix {
                return Err(prefix_conflict(&entry.task_prefix, "this machine"));
            }
        }
        if !names.insert(entry.name.to_ascii_lowercase()) {
            return Err(name_conflict(&entry.name, "another entry"));
        }
        if !machine_ids.insert(entry.machine_id.as_str()) {
            return Err(OrbitError::AmbiguousDestination(format!(
                "machine_id '{}' appears in more than one host entry",
                entry.machine_id
            )));
        }
        if !prefixes.insert(entry.task_prefix.as_str()) {
            return Err(prefix_conflict(&entry.task_prefix, "another entry"));
        }
    }
    Ok(())
}

/// The legacy file keeps the rules it always had: required keys, valid and
/// unique `machine_id`, non-blank `ssh`.
fn validate_legacy(rows: &[LegacyHost], path: &Path) -> Result<(), OrbitError> {
    let mut machine_ids = HashSet::with_capacity(rows.len());
    for row in rows {
        if !machine_ids.insert(row.machine_id.as_str()) {
            return Err(OrbitError::AmbiguousDestination(format!(
                "machine_id '{}' appears more than once in '{}'",
                row.machine_id,
                path.display()
            )));
        }
        validate_machine_id(&row.machine_id).map_err(|error| {
            OrbitError::InvalidInput(format!(
                "'{}' has invalid machine_id '{}': {error}",
                path.display(),
                row.machine_id
            ))
        })?;
        if row.ssh.trim().is_empty() {
            return Err(OrbitError::InvalidInput(format!(
                "'{}' has a blank ssh target for '{}'",
                path.display(),
                row.machine_id
            )));
        }
    }
    Ok(())
}

fn name_conflict(name: &str, holder: &str) -> OrbitError {
    OrbitError::host_registry(
        HostRegistryCode::HostNameConflict,
        format!("host name '{name}' is already used by {holder}; choose another with --name"),
    )
}

fn prefix_conflict(prefix: &str, holder: &str) -> OrbitError {
    OrbitError::host_registry(
        HostRegistryCode::TaskPrefixConflict,
        format!(
            "task prefix '{prefix}' is already used by {holder}; prefixes must be unique so a \
             task id names exactly one host"
        ),
    )
}

/// Name the file a load-time validation failure came from, keeping its code.
fn in_file(error: OrbitError, path: &Path) -> OrbitError {
    let context = |message: String| format!("'{}': {message}", path.display());
    match error {
        OrbitError::HostRegistry { code, message } => {
            OrbitError::host_registry(code, context(message))
        }
        OrbitError::AmbiguousDestination(message) => {
            OrbitError::AmbiguousDestination(context(message))
        }
        OrbitError::InvalidInput(message) => OrbitError::InvalidInput(context(message)),
        other => other,
    }
}

fn invalid_file(path: &Path, error: toml::de::Error) -> OrbitError {
    OrbitError::InvalidInput(format!("invalid host file '{}': {error}", path.display()))
}

fn utf8(bytes: &[u8], path: &Path) -> Result<String, OrbitError> {
    String::from_utf8(bytes.to_vec())
        .map_err(|_| OrbitError::InvalidInput(format!("'{}' is not UTF-8", path.display())))
}

fn read_optional(path: &Path) -> Result<Option<Vec<u8>>, OrbitError> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(OrbitError::Io(format!(
            "failed to read '{}': {error}",
            path.display()
        ))),
    }
}

/// Remove the migrated legacy file from beneath the canonical global root.
fn remove_legacy_file(global_root: &Path) -> Result<(), OrbitError> {
    let canonical_root = global_root.canonicalize().map_err(|error| {
        OrbitError::Io(format!(
            "failed to canonicalize '{}': {error}",
            global_root.display()
        ))
    })?;
    let path = canonical_root.join(LEGACY_DESTINATIONS_FILE);
    if !path.starts_with(&canonical_root) {
        return Err(OrbitError::InvalidInput(format!(
            "legacy destinations path escapes its parent: {}",
            path.display()
        )));
    }
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(OrbitError::Io(format!(
            "wrote the host file but could not remove the migrated '{}': {error}; delete it by \
             hand, since every consumer refuses while both files exist",
            path.display()
        ))),
    }
}
