//! Auto-task discovery [ORB-10149]: load `<orbit_dir>/auto_tasks/*.yaml`,
//! fail-closed per file. An invalid definition becomes a load error and is
//! treated as absent; it never fires with defaults (mirrors the routine
//! loader). The file stem must equal the definition's `name` so the on-disk
//! identity and the provenance-tag suffix stay in lockstep.

use std::path::{Path, PathBuf};

use orbit_common::protocol::yaml::parse_auto_task_yaml;
use orbit_types::workflow::AutoTaskDefinition;

/// Directory under a workspace's `.orbit/` holding auto-task YAML files.
pub const AUTO_TASKS_DIR: &str = "auto_tasks";

/// Absolute path of the auto-tasks directory for an orbit dir.
pub fn auto_tasks_dir(orbit_dir: &Path) -> PathBuf {
    orbit_dir.join(AUTO_TASKS_DIR)
}

/// Path of one definition's YAML file (`<orbit_dir>/auto_tasks/<name>.yaml`).
pub fn definition_path(orbit_dir: &Path, name: &str) -> PathBuf {
    auto_tasks_dir(orbit_dir).join(format!("{name}.yaml"))
}

/// One successfully loaded, fully validated definition.
#[derive(Debug, Clone)]
pub struct LoadedAutoTask {
    /// The parsed definition.
    pub definition: AutoTaskDefinition,
    /// Path of the YAML file it came from.
    pub path: PathBuf,
}

/// One fail-closed load failure: the definition it names is treated as absent.
#[derive(Debug, Clone)]
pub struct AutoTaskLoadError {
    /// File that failed, when the failure is file-scoped.
    pub path: Option<PathBuf>,
    /// Human-readable reason.
    pub message: String,
}

/// Result of one discovery pass.
#[derive(Debug, Default)]
pub struct AutoTaskCollection {
    /// Valid definitions, in stable filename order.
    pub definitions: Vec<LoadedAutoTask>,
    /// Everything that failed fail-closed.
    pub errors: Vec<AutoTaskLoadError>,
}

/// Load every `*.yaml` under `<orbit_dir>/auto_tasks/`, fail-closed per file.
/// A missing directory is a clean empty collection.
pub fn collect_auto_tasks(orbit_dir: &Path) -> AutoTaskCollection {
    let mut collection = AutoTaskCollection::default();
    let dir = auto_tasks_dir(orbit_dir);
    if !dir.is_dir() {
        return collection;
    }

    let mut paths: Vec<PathBuf> = match std::fs::read_dir(&dir) {
        Ok(entries) => entries
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .filter(|path| {
                path.extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| {
                        ext.eq_ignore_ascii_case("yaml") || ext.eq_ignore_ascii_case("yml")
                    })
            })
            .collect(),
        Err(error) => {
            collection.errors.push(AutoTaskLoadError {
                path: Some(dir),
                message: format!("failed to list auto_tasks directory: {error}"),
            });
            return collection;
        }
    };
    paths.sort();

    for path in paths {
        match load_definition_file(&path) {
            Ok(loaded) => collection.definitions.push(loaded),
            Err(message) => collection.errors.push(AutoTaskLoadError {
                path: Some(path),
                message,
            }),
        }
    }
    collection
}

fn load_definition_file(path: &Path) -> Result<LoadedAutoTask, String> {
    let raw = std::fs::read_to_string(path).map_err(|error| format!("read failed: {error}"))?;
    let definition = parse_auto_task_yaml(&raw).map_err(|error| error.to_string())?;

    // The file stem is the definition identity: reject a mismatch so CRUD (which
    // writes `<name>.yaml`) and the provenance tag stay consistent.
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default();
    if stem != definition.name {
        return Err(format!(
            "auto-task file stem '{stem}' does not match definition name '{}'",
            definition.name
        ));
    }

    Ok(LoadedAutoTask {
        definition,
        path: path.to_path_buf(),
    })
}
