use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::security::redaction::redact_home_dir;
use orbit_config::ConfigRoots;
use serde_json::{Value as JsonValue, json};

use crate::runtime::{CONFIG_TOML_FILE, OrbitRuntime, existing_config_file_path};

/// The resolved roots and store locations `orbit config show` prints under
/// `Paths`, as `(label, value)` rows in rendering order.
///
/// One authority for both surfaces: the CLI formats these into aligned
/// columns and the dashboard renders them as a two-column grid.
pub fn path_rows(runtime: &OrbitRuntime, config_path: Option<&Path>) -> Vec<(String, String)> {
    let mut rows: Vec<(String, String)> = vec![
        ("global root".to_string(), path_cell(&runtime.global_root())),
        (
            "workspace root".to_string(),
            path_cell(&runtime.shared_root()),
        ),
        ("local root".to_string(), path_cell(&runtime.local_root())),
    ];
    if let Some(config_path) = config_path {
        rows.push(("config file".to_string(), path_cell(config_path)));
    }
    rows.extend(persistence_rows(&runtime.persistence_config_json()));
    rows
}

/// Expand the persistence object into rows, folding resource directories that
/// share a parent into one row instead of repeating the prefix.
fn persistence_rows(persistence: &JsonValue) -> Vec<(String, String)> {
    const RESOURCE_LABELS: &[(&str, &str)] = &[
        ("activity", "activities"),
        ("executor", "executors"),
        ("job", "jobs"),
        ("policy", "policies"),
        ("skill", "skills"),
    ];
    const FILE_LABELS: &[(&str, &str)] = &[("audit", "audit db"), ("semantic", "semantic db")];

    let Some(entries) = persistence.as_object() else {
        return Vec::new();
    };
    let path_of = |name: &str| {
        entries
            .get(name)
            .and_then(|entry| entry.get("path"))
            .and_then(JsonValue::as_str)
            .map(PathBuf::from)
    };

    let mut rows = Vec::new();
    for (name, label) in FILE_LABELS {
        if let Some(path) = path_of(name) {
            rows.push(((*label).to_string(), path_cell(&path)));
        }
    }

    let mut grouped: BTreeMap<PathBuf, Vec<(&str, PathBuf)>> = BTreeMap::new();
    for (name, label) in RESOURCE_LABELS {
        if let Some(path) = path_of(name) {
            let parent = path.parent().unwrap_or(&path).to_path_buf();
            grouped.entry(parent).or_default().push((label, path));
        }
    }
    for (parent, members) in grouped {
        match members.as_slice() {
            [(label, path)] => rows.push(((*label).to_string(), path_cell(path))),
            members => rows.push((
                members
                    .iter()
                    .map(|(label, _)| *label)
                    .collect::<Vec<_>>()
                    .join(", "),
                format!("{}/", path_cell(&parent)),
            )),
        }
    }

    // Anything the persistence shape gains later is still reported, under its
    // own name, rather than silently dropped from this view.
    let known = RESOURCE_LABELS
        .iter()
        .chain(FILE_LABELS.iter())
        .map(|(name, _)| *name)
        .collect::<Vec<_>>();
    for (name, entry) in entries {
        if known.contains(&name.as_str()) {
            continue;
        }
        let value = entry
            .get("path")
            .and_then(JsonValue::as_str)
            .map(|path| path_cell(Path::new(path)))
            .unwrap_or_else(|| entry.to_string());
        rows.push((name.clone(), value));
    }
    rows
}

pub(super) fn path_rows_json(runtime: &OrbitRuntime, config_path: Option<&Path>) -> JsonValue {
    JsonValue::Array(
        path_rows(runtime, config_path)
            .into_iter()
            .map(|(label, value)| json!({"label": label, "value": value}))
            .collect(),
    )
}

pub(super) fn config_roots(runtime: &OrbitRuntime) -> ConfigRoots {
    ConfigRoots::new(runtime.global_root(), runtime.shared_root())
}

/// Whether the replace-only security keys have stopped inheriting from
/// global. A workspace file holding only `[plugin_enablement]` is not a
/// policy layer, so the exception is not in force for it either.
pub(super) fn execution_not_inherited(
    runtime: &OrbitRuntime,
    workspace_file: &ConfigLayerFile,
) -> bool {
    runtime.shared_root() != runtime.global_root()
        && orbit_config::workspace_config_sets_policy(&workspace_file.path)
}

pub(super) fn global_config_path(runtime: &OrbitRuntime) -> PathBuf {
    runtime.global_root().join(CONFIG_TOML_FILE)
}

pub(super) fn workspace_config_path(runtime: &OrbitRuntime) -> PathBuf {
    runtime.shared_root().join(CONFIG_TOML_FILE)
}

/// One physical `config.toml` layer: the path a caller sees and whether a
/// regular file is there.
///
/// Existence is probed through the runtime's validated config-root boundary
/// rather than a bare `Path::exists` on the joined path, so a root selected by
/// `?workspace=` never reaches a filesystem probe unvalidated (CodeQL
/// `rust/path-injection`). The displayed path stays the caller's spelling of
/// the root, not the canonical one, so it matches what `orbit config path`
/// prints.
pub(super) struct ConfigLayerFile {
    pub(super) path: PathBuf,
    pub(super) exists: bool,
}

impl ConfigLayerFile {
    pub(super) fn json(&self) -> JsonValue {
        json!({
            "path": path_cell(&self.path),
            "exists": self.exists,
        })
    }
}

pub(super) fn config_layer_file(root: &Path) -> Result<ConfigLayerFile, OrbitError> {
    let exists = existing_config_file_path(root)?.is_some();
    Ok(ConfigLayerFile {
        path: root.join(CONFIG_TOML_FILE),
        exists,
    })
}

pub(super) fn path_cell(path: &Path) -> String {
    redact_home_dir(&path.display().to_string())
}
