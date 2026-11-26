//! The host-owned copy of a plugin's build record
//! (`docs/design/plugins/3_install_time_build.md` §3.6).
//!
//! The record lives on the `plugins` row, which a backend holding
//! `orbit_tools` can write. The installing command also writes it to
//! `<global_root>/plugins/.grants/<ns>.build.json`, beside the grant witness
//! and outside every backend's and agent's write boundary, and the loader
//! refuses a row whose record disagrees with that copy. As with grants, the
//! copy holds because of the write boundary, not cryptography.

use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::fs::io::atomic_write_text;
use orbit_types::plugin::{InstalledPlugin, PluginBuildRecord, is_valid_namespace};
use serde::{Deserialize, Serialize};

use super::grants::plugin_grant_witness_path;

const BUILD_WITNESS_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
struct BuildWitness {
    schema_version: u32,
    plugin: String,
    record: PluginBuildRecord,
}

/// `<global_root>/plugins/.grants/<ns>.build.json`.
pub fn plugin_build_witness_path(global_root: &Path, name: &str) -> PathBuf {
    plugin_grant_witness_path(global_root, name).with_extension("build.json")
}

/// Write the witness for `record`, or remove it when the install records no
/// build. Called before the row is written, so a failed row write leaves the
/// previous row disagreeing with the new witness: refused, never trusted.
pub fn record_build_witness(
    global_root: &Path,
    name: &str,
    record: Option<&PluginBuildRecord>,
) -> Result<(), OrbitError> {
    if !is_valid_namespace(name) {
        return Err(OrbitError::Execution(format!(
            "cannot record a build witness for invalid plugin namespace '{name}'"
        )));
    }
    let path = plugin_build_witness_path(global_root, name);
    let Some(record) = record else {
        return match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(OrbitError::Io(format!(
                "remove {}: {error}",
                path.display()
            ))),
        };
    };
    let body = serde_json::to_string_pretty(&BuildWitness {
        schema_version: BUILD_WITNESS_SCHEMA_VERSION,
        plugin: name.to_string(),
        record: record.clone(),
    })
    .map_err(|error| OrbitError::Execution(format!("serialize build witness: {error}")))?;
    atomic_write_text(&path, &format!("{body}\n"))
        .map_err(|error| OrbitError::Io(format!("write {}: {error}", path.display())))
}

/// Drop the witness when the install goes away.
pub fn forget_build_witness(global_root: &Path, name: &str) {
    if !is_valid_namespace(name) {
        return;
    }
    let path = plugin_build_witness_path(global_root, name);
    if path.exists()
        && let Err(error) = std::fs::remove_file(&path)
    {
        tracing::warn!(
            target: "orbit.core.plugin",
            plugin = %name,
            path = %path.display(),
            "could not remove the build witness: {error}",
        );
    }
}

/// Check a row's build record against its witness. `Err` is the
/// operator-facing diagnostic; the caller registers the plugin inactive.
pub fn verify_build_record(global_root: &Path, installed: &InstalledPlugin) -> Result<(), String> {
    let name = &installed.name;
    if !is_valid_namespace(name) {
        return Err(format!(
            "plugin '{name}' has an invalid namespace, so its build record cannot be checked"
        ));
    }
    let path = plugin_build_witness_path(global_root, name);
    let witness = match std::fs::read_to_string(&path) {
        Ok(raw) => Some(
            serde_json::from_str::<BuildWitness>(&raw)
                .ok()
                .filter(|witness| {
                    witness.schema_version == BUILD_WITNESS_SCHEMA_VERSION
                        && witness.plugin == *name
                })
                .map(|witness| witness.record),
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(format!(
                "plugin '{name}': its build witness {} is unreadable: {error}",
                path.display()
            ));
        }
    };
    let remedy = format!(
        "reinstall it with `orbit plugin upgrade {name} {} --allow-build`",
        installed
            .build
            .as_ref()
            .map_or(installed.source.as_str(), |build| build.source.as_str())
    );
    match (&installed.build, witness) {
        (None, None) => Ok(()),
        (Some(record), Some(Some(witnessed))) if *record == witnessed => Ok(()),
        (Some(_), None) => Err(format!(
            "plugin '{name}' records a build, but its build witness {} is missing; {remedy}",
            path.display()
        )),
        (None, Some(_)) => Err(format!(
            "plugin '{name}' records no build, but a build witness {} exists for it; {remedy}",
            path.display()
        )),
        (Some(_), Some(_)) => Err(format!(
            "plugin '{name}' has a build record that does not match its build witness {}; \
             {remedy}",
            path.display()
        )),
    }
}
