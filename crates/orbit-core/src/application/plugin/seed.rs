//! Seed a plugin's routines and auto-tasks into the workspace (§3).
//!
//! The managed-asset rule applies: each file is written once with a
//! provenance header and its digest is recorded, so an upgrade re-seeds only a
//! file that still matches what the plugin last shipped. A customised file
//! gets a warning and stays exactly as the operator left it unless `--force`
//! is passed.
//!
//! The digests live beside the shipped-default manifest in their own file:
//! `.orbit-managed-assets.json` is the shipped catalog's reconciliation record
//! and rewriting it with a plugin's entries would retire every default in the
//! directory.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::fs::io::{atomic_write_text, write_text_with_parent};
use orbit_common::security::release::sha256_hex;
use orbit_tools::plugin::LoadedPlugin;
use orbit_types::workflow::{AUTO_TASK_SCHEMA_VERSION, ROUTINE_SCHEMA_VERSION};
use serde::{Deserialize, Serialize};

use crate::runtime::plugin::definitions::{
    PluginDefinitionSet, provenance_header, seeded_definition_name,
};

/// Manifest of the definitions Orbit seeded from plugins into one directory.
pub(crate) const PLUGIN_ASSET_MANIFEST_FILE: &str = ".orbit-managed-plugin-assets.json";
const PLUGIN_ASSET_MANIFEST_SCHEMA_VERSION: u32 = 1;

/// What seeding did to one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginSeedAction {
    /// The file did not exist and was written.
    Created,
    /// The file still matched what the plugin last shipped and was rewritten.
    Refreshed,
    /// The file already matches what this plugin version ships.
    Unchanged,
    /// The file differs from what Orbit wrote and was preserved.
    Customised,
}

impl PluginSeedAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Refreshed => "refreshed",
            Self::Unchanged => "unchanged",
            Self::Customised => "customised",
        }
    }
}

/// One seeded definition, as `orbit plugin enable` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginSeedOutcome {
    /// `routine` or `auto_task`.
    pub kind: &'static str,
    /// Seeded definition name (`<ns>-<name>`).
    pub name: String,
    pub path: PathBuf,
    pub action: PluginSeedAction,
    /// Set for a preserved file: what the operator has to do about it.
    pub warning: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PluginAssetManifest {
    schema_version: u32,
    #[serde(default)]
    assets: BTreeMap<String, PluginAssetRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PluginAssetRecord {
    plugin: String,
    version: String,
    /// SHA-256 of the bytes Orbit wrote.
    digest: String,
}

/// Seed every routine and auto-task `plugin` ships.
///
/// `routines_dir` and `auto_tasks_dir` are the workspace directories the
/// loaders read; the caller supplies them because the two live under different
/// roots (routines are shared, auto-tasks are per checkout).
pub fn seed_plugin_definitions(
    plugin: &LoadedPlugin,
    definitions: &PluginDefinitionSet,
    routines_dir: &Path,
    auto_tasks_dir: &Path,
    force: bool,
) -> Result<Vec<PluginSeedOutcome>, OrbitError> {
    let namespace = plugin.namespace().to_string();
    let version = plugin.manifest.metadata.version.clone();
    let mut outcomes = Vec::new();

    let mut routines: Vec<(String, String)> = Vec::new();
    for routine in &definitions.routines {
        let name = seeded_definition_name(&namespace, &routine.name);
        let mut rendered = routine.definition.clone();
        rendered.name = name.clone();
        rendered.enabled = false;
        rendered.schema_version = ROUTINE_SCHEMA_VERSION;
        let body = serde_yaml::to_string(&rendered).map_err(|error| {
            OrbitError::Execution(format!("render seeded routine '{name}': {error}"))
        })?;
        routines.push((
            name,
            format!(
                "{}{body}",
                provenance_header(&namespace, &version, "routine")
            ),
        ));
    }
    let mut auto_tasks: Vec<(String, String)> = Vec::new();
    for auto_task in &definitions.auto_tasks {
        let name = seeded_definition_name(&namespace, &auto_task.name);
        let mut rendered = auto_task.definition.clone();
        rendered.name = name.clone();
        rendered.enabled = false;
        rendered.schema_version = AUTO_TASK_SCHEMA_VERSION;
        let body = serde_yaml::to_string(&rendered).map_err(|error| {
            OrbitError::Execution(format!("render seeded auto-task '{name}': {error}"))
        })?;
        auto_tasks.push((
            name,
            format!(
                "{}{body}",
                provenance_header(&namespace, &version, "auto-task")
            ),
        ));
    }

    // Check both definition directories before writing either one. A plugin
    // can otherwise seed its routines successfully and only then discover
    // that one of its auto-task filenames is already owned by another
    // plugin, leaving a partially applied enable behind.
    refuse_cross_plugin_ownership("routine", routines_dir, &routines, &namespace)?;
    refuse_cross_plugin_ownership("auto_task", auto_tasks_dir, &auto_tasks, &namespace)?;

    outcomes.extend(write_seeded_files(
        "routine",
        routines_dir,
        &routines,
        &namespace,
        &version,
        force,
    )?);
    outcomes.extend(write_seeded_files(
        "auto_task",
        auto_tasks_dir,
        &auto_tasks,
        &namespace,
        &version,
        force,
    )?);

    Ok(outcomes)
}

fn refuse_cross_plugin_ownership(
    kind: &'static str,
    dir: &Path,
    files: &[(String, String)],
    namespace: &str,
) -> Result<(), OrbitError> {
    if files.is_empty() {
        return Ok(());
    }
    let manifest_path = dir.join(PLUGIN_ASSET_MANIFEST_FILE);
    let manifest = read_manifest(&manifest_path)?;
    for (name, _) in files {
        let file_name = format!("{name}.yaml");
        refuse_recorded_owner(
            kind,
            &dir.join(&file_name),
            manifest.assets.get(&file_name),
            namespace,
        )?;
    }
    Ok(())
}

fn write_seeded_files(
    kind: &'static str,
    dir: &Path,
    files: &[(String, String)],
    namespace: &str,
    version: &str,
    force: bool,
) -> Result<Vec<PluginSeedOutcome>, OrbitError> {
    if files.is_empty() {
        return Ok(Vec::new());
    }
    let manifest_path = dir.join(PLUGIN_ASSET_MANIFEST_FILE);
    let mut manifest = read_manifest(&manifest_path)?;
    let mut outcomes = Vec::new();

    for (name, rendered) in files {
        let file_name = format!("{name}.yaml");
        let path = dir.join(&file_name);
        let rendered_digest = sha256_hex(rendered.as_bytes());
        let recorded = manifest.assets.get(&file_name).cloned();
        refuse_recorded_owner(kind, &path, recorded.as_ref(), namespace)?;

        let action = if !path.exists() {
            write_text_with_parent(&path, rendered)?;
            PluginSeedAction::Created
        } else {
            let existing = std::fs::read_to_string(&path).map_err(|error| {
                OrbitError::Io(format!("read seeded {kind} '{}': {error}", path.display()))
            })?;
            let existing_digest = sha256_hex(existing.as_bytes());
            let orbit_written = recorded.as_ref().is_some_and(|record| {
                record.plugin == namespace && record.digest == existing_digest
            });
            if existing_digest == rendered_digest {
                PluginSeedAction::Unchanged
            } else if orbit_written || force {
                write_text_with_parent(&path, rendered)?;
                PluginSeedAction::Refreshed
            } else {
                PluginSeedAction::Customised
            }
        };

        let warning = (action == PluginSeedAction::Customised).then(|| {
            format!(
                "seeded {kind} '{}' was edited after Orbit wrote it, so plugin '{namespace}' \
                 v{version} did not overwrite it; review the difference and re-run \
                 `orbit plugin enable {namespace} --force` to take the plugin's version",
                path.display()
            )
        });
        if let Some(warning) = &warning {
            tracing::warn!(target: "orbit.core.plugins", warning, "seeded plugin definition preserved");
        } else {
            manifest.assets.insert(
                file_name,
                PluginAssetRecord {
                    plugin: namespace.to_string(),
                    version: version.to_string(),
                    digest: rendered_digest,
                },
            );
        }
        outcomes.push(PluginSeedOutcome {
            kind,
            name: name.clone(),
            path,
            action,
            warning,
        });
    }

    write_manifest(&manifest_path, &manifest)?;
    Ok(outcomes)
}

fn refuse_recorded_owner(
    kind: &'static str,
    path: &Path,
    recorded: Option<&PluginAssetRecord>,
    namespace: &str,
) -> Result<(), OrbitError> {
    let Some(recorded) = recorded else {
        return Ok(());
    };
    if recorded.plugin == namespace {
        return Ok(());
    }
    Err(OrbitError::InvalidInput(format!(
        "plugin '{namespace}' cannot seed {kind} '{}': the managed file is owned by plugin '{}'; \
         rename one definition so its seeded filename is unique",
        path.display(),
        recorded.plugin
    )))
}

fn read_manifest(path: &Path) -> Result<PluginAssetManifest, OrbitError> {
    if !path.exists() {
        return Ok(PluginAssetManifest {
            schema_version: PLUGIN_ASSET_MANIFEST_SCHEMA_VERSION,
            assets: BTreeMap::new(),
        });
    }
    let raw = std::fs::read_to_string(path).map_err(|error| {
        OrbitError::Io(format!(
            "read plugin asset manifest '{}': {error}",
            path.display()
        ))
    })?;
    let manifest: PluginAssetManifest = serde_json::from_str(&raw).map_err(|error| {
        OrbitError::InvalidInput(format!(
            "plugin asset manifest '{}' is invalid: {error}; repair it or move it aside after \
             reviewing the seeded definitions it tracks",
            path.display()
        ))
    })?;
    if manifest.schema_version != PLUGIN_ASSET_MANIFEST_SCHEMA_VERSION {
        return Err(OrbitError::InvalidInput(format!(
            "plugin asset manifest '{}' uses unsupported schemaVersion {}; expected {}",
            path.display(),
            manifest.schema_version,
            PLUGIN_ASSET_MANIFEST_SCHEMA_VERSION
        )));
    }
    Ok(manifest)
}

fn write_manifest(path: &Path, manifest: &PluginAssetManifest) -> Result<(), OrbitError> {
    let mut encoded = serde_json::to_string_pretty(manifest)
        .map_err(|error| OrbitError::Store(format!("serialize plugin asset manifest: {error}")))?;
    encoded.push('\n');
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| OrbitError::Io(format!("create {}: {error}", parent.display())))?;
    }
    atomic_write_text(path, &encoded)
        .map_err(|error| OrbitError::Io(format!("write {}: {error}", path.display())))
}
