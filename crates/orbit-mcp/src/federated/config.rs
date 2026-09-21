//! Operator-configured federated destinations and the machine-qualified selector.
//!
//! Remote membership is the operator file [`DESTINATIONS_FILE`]. Local
//! membership is implicit: the accepting machine is always a destination,
//! keyed by its stable `machine_id`, and is never declared as an SSH row.

use std::collections::HashSet;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use orbit_common::OrbitError;
use orbit_types::identity::{validate_machine_id, validate_registry_identifier};
use serde::Deserialize;

pub const DESTINATIONS_FILE: &str = "mcp-destinations.toml";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineQualifiedSelector {
    machine_id: String,
    workspace_id: String,
}

impl MachineQualifiedSelector {
    pub fn machine_id(&self) -> &str {
        &self.machine_id
    }

    pub fn workspace_id(&self) -> &str {
        &self.workspace_id
    }
}

impl FromStr for MachineQualifiedSelector {
    type Err = OrbitError;

    fn from_str(token: &str) -> Result<Self, Self::Err> {
        let (machine_id, workspace_id) = token
            .split_once('/')
            .ok_or_else(|| unknown_selector(token))?;
        validate_machine_id(machine_id).map_err(|_| unknown_selector(token))?;
        validate_workspace_id(workspace_id).map_err(|_| unknown_selector(token))?;
        Ok(Self {
            machine_id: machine_id.to_string(),
            workspace_id: workspace_id.to_string(),
        })
    }
}

impl fmt::Display for MachineQualifiedSelector {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}/{}", self.machine_id, self.workspace_id)
    }
}

fn validate_workspace_id(workspace_id: &str) -> Result<(), ()> {
    validate_registry_identifier("workspace_id", workspace_id).map_err(|_| ())?;
    let suffix = workspace_id.strip_prefix("ws_").ok_or(())?;
    if suffix.is_empty()
        || !suffix
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(());
    }
    Ok(())
}

fn unknown_selector(token: &str) -> OrbitError {
    OrbitError::UnknownSelector(token.to_string())
}

/// How the mux reaches one destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DestinationTransport {
    /// The accepting machine. Listed and routed in-process; never over SSH.
    Local { machine_name: String },
    /// An operator-configured SSH remote.
    Ssh { target: String },
}

/// One federated destination the mux may list and route to.
///
/// Local membership is composed at serve time. SSH rows come only from
/// [`DestinationsFile`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Destination {
    pub machine_id: String,
    pub transport: DestinationTransport,
}

impl Destination {
    pub fn local(machine_id: impl Into<String>, machine_name: impl Into<String>) -> Self {
        Self {
            machine_id: machine_id.into(),
            transport: DestinationTransport::Local {
                machine_name: machine_name.into(),
            },
        }
    }

    pub fn ssh(target: impl Into<String>, machine_id: impl Into<String>) -> Self {
        Self {
            machine_id: machine_id.into(),
            transport: DestinationTransport::Ssh {
                target: target.into(),
            },
        }
    }

    pub fn is_local(&self) -> bool {
        matches!(self.transport, DestinationTransport::Local { .. })
    }

    /// Display identity attributed to a descriptor's `machine_name` field.
    ///
    /// Local destinations use the accepting machine's `machine.name`. Remotes
    /// use the operator's configured SSH target — the v1 discovery envelope
    /// still carries no display name.
    pub fn machine_name_display(&self) -> &str {
        match &self.transport {
            DestinationTransport::Local { machine_name } => machine_name,
            DestinationTransport::Ssh { target } => target,
        }
    }

    pub fn ssh_target(&self) -> Option<&str> {
        match &self.transport {
            DestinationTransport::Ssh { target } => Some(target),
            DestinationTransport::Local { .. } => None,
        }
    }
}

/// One operator-configured SSH remote from [`DESTINATIONS_FILE`].
///
/// Local workspaces need no row. A machine-id-only row is still invalid: `ssh`
/// is required on every configured destination.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteDestination {
    pub ssh: String,
    pub machine_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DestinationsFile {
    #[serde(default)]
    pub destinations: Vec<RemoteDestination>,
}

pub fn destinations_path(global_orbit_root: &Path) -> PathBuf {
    global_orbit_root.join(DESTINATIONS_FILE)
}

/// Load configured SSH remotes.
///
/// A missing file or an empty `destinations` list is a valid local-only
/// configuration. Invalid rows still fail closed before the mux advertises
/// tools.
pub fn load_destinations(path: &Path) -> Result<DestinationsFile, OrbitError> {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(DestinationsFile {
                destinations: Vec::new(),
            });
        }
        Err(error) => {
            return Err(OrbitError::Io(format!(
                "failed to read federated MCP destinations '{}': {error}",
                path.display()
            )));
        }
    };
    let destinations: DestinationsFile = toml::from_str(&contents).map_err(|error| {
        OrbitError::InvalidInput(format!(
            "invalid federated MCP destinations '{}': {error}",
            path.display()
        ))
    })?;
    validate_destinations(&destinations, path)?;
    Ok(destinations)
}

/// Compose the mux membership: implicit local destination first, then every
/// configured remote whose `machine_id` is not the accepting machine.
///
/// An explicit SSH row that already names the local machine is dropped rather
/// than duplicated. The local in-process route is the one route for that
/// identity.
pub fn federated_membership(
    local_machine_id: impl Into<String>,
    local_machine_name: impl Into<String>,
    remotes: DestinationsFile,
) -> Vec<Destination> {
    let local_machine_id = local_machine_id.into();
    let local = Destination::local(local_machine_id.clone(), local_machine_name);
    let remotes = remotes
        .destinations
        .into_iter()
        .filter(|remote| remote.machine_id != local_machine_id)
        .map(|remote| Destination::ssh(remote.ssh, remote.machine_id));
    std::iter::once(local).chain(remotes).collect()
}

fn validate_destinations(destinations: &DestinationsFile, path: &Path) -> Result<(), OrbitError> {
    let mut machine_ids = HashSet::with_capacity(destinations.destinations.len());
    for destination in &destinations.destinations {
        if !machine_ids.insert(destination.machine_id.as_str()) {
            return Err(OrbitError::AmbiguousDestination(format!(
                "machine_id '{}' appears more than once in '{}'",
                destination.machine_id,
                path.display()
            )));
        }
    }
    for destination in &destinations.destinations {
        validate_machine_id(&destination.machine_id).map_err(|error| {
            OrbitError::InvalidInput(format!(
                "federated MCP destinations '{}' has invalid machine_id '{}': {error}",
                path.display(),
                destination.machine_id
            ))
        })?;
        if destination.ssh.trim().is_empty() {
            return Err(OrbitError::InvalidInput(format!(
                "federated MCP destinations '{}' has a blank ssh target for '{}'",
                path.display(),
                destination.machine_id
            )));
        }
    }
    Ok(())
}
