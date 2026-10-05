//! Layout migration step implementations.

use std::fs;
use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::fs::io::atomic_write_text;
use orbit_types::task::is_valid_orb_task_id;

/// v1 baseline: the current `.orbit/` shape. Intentionally a no-op — running
/// it on any existing workspace changes nothing and then records version 1,
/// which is how pre-versioning workspaces adopt the marker.
pub(super) fn apply_baseline_layout(_orbit_dir: &Path) -> Result<(), OrbitError> {
    Ok(())
}

/// v2: `TaskStatus::Friction` was removed in ORB-10202, so a persisted task
/// carrying that status no longer deserializes. Preserve the record but keep
/// it out of active work by mapping the removed status to `archived` in both
/// the task envelope and its event history.
///
/// Task projections may be symlinks into the canonical global bundle store.
/// `Path::is_dir` deliberately follows those direct projection entries; the
/// migration never recurses beyond one task bundle.
pub(super) fn apply_archive_friction_tasks(orbit_dir: &Path) -> Result<(), OrbitError> {
    let tasks_dir = orbit_dir.join("tasks");
    let metadata = match fs::symlink_metadata(&tasks_dir) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(layout_io_error(
                "inspect projected task directory",
                &tasks_dir,
                error,
            ));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Ok(());
    }

    let entries = match fs::read_dir(&tasks_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(layout_io_error(
                "read projected task directory",
                &tasks_dir,
                error,
            ));
        }
    };

    for entry in entries {
        let entry = entry.map_err(|error| {
            layout_io_error("read projected task directory entry", &tasks_dir, error)
        })?;
        let bundle_dir = entry.path();
        let is_task_bundle = bundle_dir
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(is_valid_orb_task_id);
        if !is_task_bundle || !bundle_dir.is_dir() {
            continue;
        }

        migrate_event_statuses(&bundle_dir.join("events.jsonl"))?;
        migrate_envelope_status(&bundle_dir.join("task.yaml"))?;
    }
    Ok(())
}

/// v3: checkout-local task symlinks were a disposable convenience view over
/// canonical bundles. Remove only links whose names and targets match the
/// shipped `<global>/tasks/workspaces/<workspace>/<task-id>` shape. Anything
/// ambiguous is retained and diagnosed; in particular, never traverse a
/// symlink used as the `.orbit/tasks` parent.
pub(super) fn remove_legacy_task_projections(orbit_dir: &Path) -> Result<(), OrbitError> {
    let tasks_dir = orbit_dir.join("tasks");
    let metadata = match fs::symlink_metadata(&tasks_dir) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(layout_io_error(
                "inspect legacy task directory",
                &tasks_dir,
                error,
            ));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        orbit_common::tracing::warn!(
            target: "orbit.store.layout",
            path = %tasks_dir.display(),
            "retained ambiguous legacy task path; expected a real directory",
        );
        return Ok(());
    }

    let entries = fs::read_dir(&tasks_dir)
        .map_err(|error| layout_io_error("read legacy task directory", &tasks_dir, error))?;
    for entry in entries {
        let entry = entry.map_err(|error| {
            layout_io_error("read legacy task directory entry", &tasks_dir, error)
        })?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| layout_io_error("inspect legacy task entry", &path, error))?;
        if !metadata.file_type().is_symlink() {
            continue;
        }
        let target = fs::read_link(&path)
            .map_err(|error| layout_io_error("read legacy task link", &path, error))?;
        if !is_owned_task_projection(&path, &target) {
            orbit_common::tracing::warn!(
                target: "orbit.store.layout",
                path = %path.display(),
                target = %target.display(),
                "retained unrecognized link in legacy task directory",
            );
            continue;
        }
        fs::remove_file(&path)
            .map_err(|error| layout_io_error("remove legacy task link", &path, error))?;
    }

    match fs::remove_dir(&tasks_dir) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::DirectoryNotEmpty => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(layout_io_error(
            "remove empty legacy task directory",
            &tasks_dir,
            error,
        )),
    }
}

fn is_owned_task_projection(link: &Path, target: &Path) -> bool {
    let Some(task_id) = link.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    if !is_valid_orb_task_id(task_id) || !target.is_absolute() {
        return false;
    }
    let Some(workspace_dir) = target.parent() else {
        return false;
    };
    let Some(workspaces_dir) = workspace_dir.parent() else {
        return false;
    };
    let Some(tasks_dir) = workspaces_dir.parent() else {
        return false;
    };

    target.file_name() == link.file_name()
        && workspace_dir.file_name().is_some()
        && workspaces_dir
            .file_name()
            .is_some_and(|name| name == "workspaces")
        && tasks_dir.file_name().is_some_and(|name| name == "tasks")
}

fn migrate_envelope_status(path: &Path) -> Result<(), OrbitError> {
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(layout_io_error("read task envelope", path, error)),
    };
    let mut value: serde_yaml::Value = serde_yaml::from_str(&raw).map_err(|error| {
        OrbitError::Migration(format!(
            "cannot parse task envelope '{}' while archiving removed friction status: {error}",
            path.display()
        ))
    })?;
    let Some(mapping) = value.as_mapping_mut() else {
        return Err(OrbitError::Migration(format!(
            "task envelope '{}' is not a YAML mapping",
            path.display()
        )));
    };
    if !replace_string_field(mapping, "status", "friction", "archived") {
        return Ok(());
    }

    let migrated = serde_yaml::to_string(&value).map_err(|error| {
        OrbitError::Migration(format!(
            "cannot serialize migrated task envelope '{}': {error}",
            path.display()
        ))
    })?;
    atomic_write_text(path, &migrated)
        .map_err(|error| layout_io_error("rewrite task envelope", path, error))
}

fn migrate_event_statuses(path: &Path) -> Result<(), OrbitError> {
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(layout_io_error("read task event history", path, error)),
    };
    let mut migrated = String::with_capacity(raw.len());
    let mut changed = false;

    for chunk in raw.split_inclusive('\n') {
        // An unterminated final row is a crash tail ignored by the bundle
        // reader. Preserve it byte-for-byte so this migration does not turn a
        // partial append into an apparently committed event.
        if !chunk.ends_with('\n') {
            migrated.push_str(chunk);
            continue;
        }
        let line = chunk
            .strip_suffix('\n')
            .unwrap_or(chunk)
            .strip_suffix('\r')
            .unwrap_or_else(|| chunk.strip_suffix('\n').unwrap_or(chunk));
        let mut value: serde_json::Value = serde_json::from_str(line).map_err(|error| {
            OrbitError::Migration(format!(
                "cannot parse task event history row at '{}': {error}",
                path.display()
            ))
        })?;
        let row_changed = value.as_object_mut().is_some_and(|object| {
            replace_json_string_field(object, "from_status", "friction", "archived")
                | replace_json_string_field(object, "to_status", "friction", "archived")
        });
        if row_changed {
            migrated.push_str(&serde_json::to_string(&value).map_err(|error| {
                OrbitError::Migration(format!(
                    "cannot serialize migrated task event history '{}': {error}",
                    path.display()
                ))
            })?);
            if chunk.ends_with("\r\n") {
                migrated.push('\r');
            }
            migrated.push('\n');
            changed = true;
        } else {
            migrated.push_str(chunk);
        }
    }

    if !changed {
        return Ok(());
    }
    atomic_write_text(path, &migrated)
        .map_err(|error| layout_io_error("rewrite task event history", path, error))
}

fn replace_string_field(
    mapping: &mut serde_yaml::Mapping,
    field: &str,
    old: &str,
    new: &str,
) -> bool {
    let key = serde_yaml::Value::String(field.to_string());
    let Some(value) = mapping.get_mut(&key) else {
        return false;
    };
    if value.as_str() != Some(old) {
        return false;
    }
    *value = serde_yaml::Value::String(new.to_string());
    true
}

fn replace_json_string_field(
    object: &mut serde_json::Map<String, serde_json::Value>,
    field: &str,
    old: &str,
    new: &str,
) -> bool {
    let Some(value) = object.get_mut(field) else {
        return false;
    };
    if value.as_str() != Some(old) {
        return false;
    }
    *value = serde_json::Value::String(new.to_string());
    true
}

fn layout_io_error(operation: &str, path: &Path, error: std::io::Error) -> OrbitError {
    OrbitError::Migration(format!("{operation} '{}': {error}", path.display()))
}
