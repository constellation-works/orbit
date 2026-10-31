//! Auto-task discovery [ORB-10149]: load `<orbit_dir>/auto_tasks/*.yaml`,
//! fail-closed per file. An invalid definition becomes a load error and is
//! treated as absent; it never fires with defaults (mirrors the routine
//! loader). The file stem must equal the definition's `name` so the on-disk
//! identity and the provenance-tag suffix stay in lockstep. Each body is
//! loaded with its operator settings ([`super::settings`]) applied; an
//! unreadable settings table fails every definition closed, since any of them
//! may carry settings.

use std::path::{Path, PathBuf};
use std::{fmt, fs};

use std::collections::BTreeMap;

use chrono::Utc;
use orbit_common::protocol::yaml::parse_auto_task_yaml;
use orbit_types::workflow::automation::ConditionScope;
use orbit_types::workflow::{AutoTaskDefinition, AutoTaskSchedule, auto_task_tag};

use super::schedule::validate_schedule;
use super::settings::{AutoTaskSettingsTable, load_settings_table};
use crate::condition::{
    BoardFacts, CheckedCondition, ConditionCarrier, ConditionEvaluator, ConditionFacts,
    OriginFacts, SelfFacts, TaskFacts, Truth, check_condition, check_guard,
};

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

/// Why a catalog directory could not be admitted for discovery.
#[derive(Debug)]
pub enum AutoTasksDirectoryError {
    /// The Orbit root or its auto-task directory does not exist.
    Missing,
    /// The directory could not be resolved or is outside its expected location.
    Invalid(String),
}

/// Resolve the configured auto-task directory before it reaches `read_dir`.
///
/// The runtime supplies the Orbit root, while the directory name is fixed by
/// this module. Canonicalizing both components and requiring the exact direct
/// child prevents a symlinked `auto_tasks` directory from redirecting a scan.
/// Inspecting the leaf first also rejects dangling links instead of treating
/// them as an absent catalog that permits shared-catalog fallback.
pub fn validated_auto_tasks_dir(orbit_dir: &Path) -> Result<PathBuf, AutoTasksDirectoryError> {
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
    if fs::symlink_metadata(&expected_dir).is_ok_and(|metadata| !metadata.is_dir()) {
        return Err(AutoTasksDirectoryError::Invalid(format!(
            "auto_tasks directory must be a regular directory directly under {}",
            canonical_orbit_dir.display()
        )));
    }
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

    let settings = load_settings_table(&dir);
    for path in paths {
        let loaded = settings
            .as_ref()
            .map_err(Clone::clone)
            .and_then(|settings| load_definition_with_settings(&path, settings));
        match loaded {
            Ok(loaded) => collection.definitions.push(loaded),
            Err(message) => collection.errors.push(AutoTaskLoadError {
                path: Some(path),
                message,
            }),
        }
    }
    collection
}

/// Load and validate one definition file with its settings applied, as
/// discovery does for each entry.
pub(crate) fn load_definition_file(path: &Path) -> Result<LoadedAutoTask, String> {
    let settings = load_settings_table(path.parent().unwrap_or(Path::new(".")))?;
    load_definition_with_settings(path, &settings)
}

fn load_definition_with_settings(
    path: &Path,
    settings: &AutoTaskSettingsTable,
) -> Result<LoadedAutoTask, String> {
    let raw = std::fs::read_to_string(path).map_err(|error| format!("read failed: {error}"))?;
    let mut definition = parse_auto_task_yaml(&raw).map_err(|error| error.to_string())?;
    if let Some(entry) = settings.get(&definition.name) {
        entry.apply(&mut definition);
        definition
            .validate()
            .map_err(|error| format!("auto-task settings for '{}': {error}", definition.name))?;
    }
    validate_schedule(&definition.schedule).map_err(|error| error.to_string())?;
    check_auto_task_conditions(&definition)?;

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

/// Type-check a definition's condition or `when:` guard against its schema,
/// and refuse a `task` condition that would match its own template's output
/// (`docs/design/condition-triggers/1_scope.md` §8). A refused expression
/// names its clause and position; a self-match names the definition and
/// `schedule.condition.when`.
pub fn check_auto_task_conditions(definition: &AutoTaskDefinition) -> Result<(), String> {
    let schedule = &definition.schedule;
    let checked = if let AutoTaskSchedule::Condition { condition } = schedule {
        Some(
            check_condition(condition, ConditionCarrier::AutoTask)
                .map_err(|error| error.to_string())?,
        )
    } else {
        None
    };
    if let Some(when) = schedule.guard().and_then(|guard| guard.when.as_ref()) {
        check_guard(when, "schedule.when", ConditionCarrier::AutoTask)
            .map_err(|error| error.to_string())?;
    }
    if let Some(checked) = checked.as_ref() {
        reject_self_match(definition, checked)?;
    }
    Ok(())
}

/// A `task` condition evaluated against the task its own template would mint.
/// The subject carries the template's status, tags (plus the definition's
/// provenance tag), title, type, priority and complexity, and the crew the
/// mint would write: a template still naming the retired `system` crew maps
/// to no crew under the default crew set, mirroring the mint's
/// `crew_unless_retired` ([ORB-15663]); the id and timestamps are load-time
/// placeholders, dependencies are empty and the parent is absent. Board and
/// self use default facts, so a board read is unknown and only a definite
/// true is an error. Origin is evaluated both absent and with a placeholder
/// mint origin, and only a true under both shapes is a self-match, so
/// origin-dependent definitions keep loading.
fn reject_self_match(
    definition: &AutoTaskDefinition,
    checked: &CheckedCondition,
) -> Result<(), String> {
    if checked.scope != ConditionScope::Task {
        return Ok(());
    }
    let now = Utc::now();
    let template = &definition.template;
    let mut tags = template.tags.clone();
    let provenance = auto_task_tag(&definition.name);
    if !tags.contains(&provenance) {
        tags.push(provenance);
    }
    let base = TaskFacts {
        id: format!("__auto_task_template__:{}", definition.name),
        title: template.title.clone(),
        status: persisted_name(&template.status),
        tags,
        task_type: persisted_name(&template.task_type),
        priority: persisted_name(&template.priority),
        complexity: template
            .complexity
            .as_ref()
            .map(persisted_name)
            .unwrap_or_else(|| "unassessed".to_string()),
        // Mirror the mint (`template_params` maps the template crew through
        // `crew_unless_retired`): under the default crew set no
        // `[crews.system]` table exists, so a template still naming the
        // retired `system` crew (see `orbit_config::RETIRED_SYSTEM_CREW`)
        // mints an unassigned task whose facts omit `task.crew`. The loader
        // sees no configured crews, so it assumes that default set here.
        crew: match template.crew.as_deref() {
            Some(crew) if crew.trim() == "system" => None,
            other => other.map(str::to_string),
        },
        parent_id: None,
        dependencies: Vec::new(),
        created_at: now,
        updated_at: now,
        status_since: now,
        origin: None,
    };
    let facts = ConditionFacts {
        now,
        board: BoardFacts::default(),
        tasks: BTreeMap::new(),
        self_facts: SelfFacts::default(),
    };
    let evaluator = ConditionEvaluator::new(&facts);
    if evaluator.truth(&checked.when, Some(&base)) != Truth::True {
        return Ok(());
    }
    let with_origin = TaskFacts {
        origin: Some(OriginFacts {
            trigger: format!("auto-task:{}", definition.name),
            fire_id: "__self_match__".to_string(),
            depth: 1,
        }),
        ..base
    };
    if evaluator.truth(&checked.when, Some(&with_origin)) != Truth::True {
        return Ok(());
    }
    Err(format!(
        "auto-task '{}': schedule.condition.when would match its own template output",
        definition.name
    ))
}

/// A value's persisted spelling, as the condition schema reads it.
fn persisted_name<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}
