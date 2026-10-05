use std::fs;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use serde::{Deserialize, Serialize};

use crate::fs::path_safety::validate_partition_id;
use crate::fs::yaml::{parse_yaml_with, write_yaml_atomic_with};

use crate::contracts::WorkspaceConfig;

const CONFIG_SCHEMA_VERSION: u32 = 1;
const CONFIG_FILE_NAME: &str = "config.yaml";

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkspaceConfigDoc {
    schema_version: u32,
    workspace_id: String,
}

pub fn workspace_config_path(orbit_dir: &Path) -> PathBuf {
    orbit_dir.join(CONFIG_FILE_NAME)
}

pub fn read_workspace_config(orbit_dir: &Path) -> Result<WorkspaceConfig, OrbitError> {
    read_workspace_config_optional(orbit_dir)?.ok_or_else(|| {
        OrbitError::InvalidInput(format!(
            "workspace config is missing: {}",
            workspace_config_path(orbit_dir).display()
        ))
    })
}

pub fn workspace_id_for_orbit_dir(orbit_dir: &Path) -> Result<String, OrbitError> {
    let config = read_workspace_config(orbit_dir).map_err(|err| match err {
        OrbitError::InvalidInput(message) => {
            OrbitError::InvalidInput(format!("{message} (expected key `workspace_id`)"))
        }
        other => other,
    })?;
    Ok(config.workspace_id)
}

pub fn read_workspace_config_optional(
    orbit_dir: &Path,
) -> Result<Option<WorkspaceConfig>, OrbitError> {
    // Workspace paths may contain aliases and symlinks, so resolve the fixed
    // config filename before reading and reject any path that escapes the
    // resolved orbit directory.
    let canonical_orbit_dir = match orbit_dir.canonicalize() {
        Ok(path) => path,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(OrbitError::Io(err.to_string())),
    };
    let candidate = canonical_orbit_dir.join(CONFIG_FILE_NAME);
    let canonical_config_path = match candidate.canonicalize() {
        Ok(path) => path,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(OrbitError::Io(err.to_string())),
    };

    if !canonical_config_path.starts_with(&canonical_orbit_dir) {
        return Err(OrbitError::InvalidInput(
            "workspace config must remain within the orbit directory".to_string(),
        ));
    }

    let raw = match fs::read_to_string(&canonical_config_path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(OrbitError::Io(err.to_string())),
    };
    let doc: WorkspaceConfigDoc = parse_yaml_with(&raw, &canonical_config_path, |_, e| {
        OrbitError::InvalidInput(format!(
            "invalid workspace config '{}': {e}",
            canonical_config_path.display()
        ))
    })?;
    validate_workspace_config_doc(doc).map(Some)
}

pub fn write_workspace_config(
    orbit_dir: &Path,
    config: &WorkspaceConfig,
) -> Result<(), OrbitError> {
    let workspace_id = validate_partition_id(&config.workspace_id)?;
    if config.schema_version != CONFIG_SCHEMA_VERSION {
        return Err(OrbitError::InvalidInput(format!(
            "unsupported workspace config schema_version {}",
            config.schema_version
        )));
    }

    let doc = WorkspaceConfigDoc {
        schema_version: CONFIG_SCHEMA_VERSION,
        workspace_id,
    };
    write_yaml_atomic_with(&workspace_config_path(orbit_dir), &doc, |e| {
        OrbitError::Store(e.to_string())
    })
}

fn validate_workspace_config_doc(doc: WorkspaceConfigDoc) -> Result<WorkspaceConfig, OrbitError> {
    if doc.schema_version != CONFIG_SCHEMA_VERSION {
        return Err(OrbitError::InvalidInput(format!(
            "unsupported workspace config schema_version {}",
            doc.schema_version
        )));
    }
    Ok(WorkspaceConfig {
        schema_version: doc.schema_version,
        workspace_id: validate_partition_id(&doc.workspace_id)?,
    })
}
