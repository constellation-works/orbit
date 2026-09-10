//! Auto-task discovery [ORB-10149]: load `<orbit_dir>/auto_tasks/*.yaml`,
//! fail-closed per file. An invalid definition becomes a load error and is
//! treated as absent; it never fires with defaults (mirrors the routine
//! loader). The file stem must equal the definition's `name` so the on-disk
//! identity and the provenance-tag suffix stay in lockstep.

use std::path::{Path, PathBuf};
use std::{fmt, fs};

use orbit_common::protocol::yaml::parse_auto_task_yaml;
use orbit_types::workflow::AutoTaskDefinition;

use super::schedule::validate_schedule;

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

#[derive(Debug)]
enum AutoTasksDirectoryError {
    Missing,
    Invalid(String),
}

/// Resolve the configured auto-task directory before it reaches `read_dir`.
///
/// The runtime supplies the Orbit root, while the directory name is fixed by
/// this module. Canonicalizing both components and requiring the exact direct
/// child prevents a symlinked `auto_tasks` directory from redirecting a scan.
fn validated_auto_tasks_dir(orbit_dir: &Path) -> Result<PathBuf, AutoTasksDirectoryError> {
    let canonical_orbit_dir = match fs::canonicalize(orbit_dir) {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(AutoTasksDirectoryError::Missing);
        }
        Err(error) => {
            return Err(AutoTasksDirectoryError::Invalid(format!(
                "failed to resolve Orbit directory {}: {error}",
                orbit_dir.display()
            )));
        }
    };
    let expected_dir = canonical_orbit_dir.join(AUTO_TASKS_DIR);
    let canonical_dir = match fs::canonicalize(&expected_dir) {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(AutoTasksDirectoryError::Missing);
        }
        Err(error) => {
            return Err(AutoTasksDirectoryError::Invalid(format!(
                "failed to resolve auto_tasks directory {}: {error}",
                expected_dir.display()
            )));
        }
    };

    if canonical_dir != expected_dir || !canonical_dir.is_dir() {
        return Err(AutoTasksDirectoryError::Invalid(format!(
            "auto_tasks directory must be a regular directory directly under {}",
            canonical_orbit_dir.display()
        )));
    }

    Ok(canonical_dir)
}

impl fmt::Display for AutoTasksDirectoryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => formatter.write_str("auto_tasks directory is missing"),
            Self::Invalid(message) => formatter.write_str(message),
        }
    }
}

/// Load every `*.yaml` under `<orbit_dir>/auto_tasks/`, fail-closed per file.
/// A missing directory is a clean empty collection.
pub fn collect_auto_tasks(orbit_dir: &Path) -> AutoTaskCollection {
    let mut collection = AutoTaskCollection::default();
    let dir = match validated_auto_tasks_dir(orbit_dir) {
        Ok(dir) => dir,
        Err(AutoTasksDirectoryError::Missing) => return collection,
        Err(error) => {
            collection.errors.push(AutoTaskLoadError {
                path: Some(auto_tasks_dir(orbit_dir)),
                message: error.to_string(),
            });
            return collection;
        }
    };

    let mut paths = Vec::new();
    match fs::read_dir(&dir) {
        Ok(entries) => {
            for entry in entries {
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(error) => {
                        collection.errors.push(AutoTaskLoadError {
                            path: Some(dir.clone()),
                            message: format!("failed to read auto_tasks directory entry: {error}"),
                        });
                        continue;
                    }
                };
                let path = entry.path();
                let metadata = match fs::symlink_metadata(&path) {
                    Ok(metadata) => metadata,
                    Err(error) => {
                        collection.errors.push(AutoTaskLoadError {
                            path: Some(path),
                            message: format!("failed to inspect auto-task entry: {error}"),
                        });
                        continue;
                    }
                };
                if !metadata.file_type().is_file() {
                    continue;
                }
                if path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| {
                        ext.eq_ignore_ascii_case("yaml") || ext.eq_ignore_ascii_case("yml")
                    })
                {
                    paths.push(path);
                }
            }
        }
        Err(error) => {
            collection.errors.push(AutoTaskLoadError {
                path: Some(dir),
                message: format!("failed to list auto_tasks directory: {error}"),
            });
            return collection;
        }
    }
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
    validate_schedule(&definition.schedule).map_err(|error| error.to_string())?;

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
