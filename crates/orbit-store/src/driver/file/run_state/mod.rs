use std::collections::btree_map::Entry;
use std::fs;
use std::path::{Component, Path, PathBuf};

use chrono::Utc;
use orbit_common::{NotFoundKind, OrbitError};
use orbit_types::workflow::PipelineState;
use serde_json::Value;

use crate::fs::path_safety::validate_path_stem;
use orbit_common::fs::io::atomic_write_text_volatile as write_atomic;

pub fn resolve_active_run_state_dir(
    orbit_root: &Path,
    run_id: &str,
) -> Result<Option<PathBuf>, OrbitError> {
    validate_run_id(run_id)?;
    let Some(runs_root) = canonical_runs_root(orbit_root)? else {
        return Ok(None);
    };
    for entry in fs::read_dir(&runs_root).map_err(|error| OrbitError::Io(error.to_string()))? {
        let entry = entry.map_err(|error| OrbitError::Io(error.to_string()))?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(job_id) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        if job_id == "archived" {
            continue;
        }
        let run_dir = path.join(run_id);
        if run_dir.is_dir() {
            let run_dir = canonicalize_dir(&run_dir, "job run state directory")?;
            ensure_under_runs_root(&run_dir, &runs_root)?;
            return Ok(Some(run_dir));
        }
    }
    Ok(None)
}

pub fn validate_active_run_state_dir(
    orbit_root: &Path,
    state_dir: &Path,
    run_id: &str,
) -> Result<PathBuf, OrbitError> {
    validate_run_id(run_id)?;
    if path_contains_parent_dir(state_dir) {
        return Err(OrbitError::InvalidInput(
            "`state_dir` must not contain `..` path components".to_string(),
        ));
    }

    let runs_root = canonical_runs_root(orbit_root)?.ok_or_else(|| {
        OrbitError::InvalidInput(format!(
            "job-runs root '{}' must exist",
            orbit_root.join("state").join("job-runs").display()
        ))
    })?;
    let state_dir = canonicalize_dir(state_dir, "state directory")?;
    ensure_under_runs_root(&state_dir, &runs_root)?;

    let expected = resolve_active_run_state_dir(orbit_root, run_id)?
        .ok_or_else(|| OrbitError::not_found(NotFoundKind::JobRun, run_id.to_string()))?;
    if state_dir != expected {
        return Err(OrbitError::InvalidInput(format!(
            "`state_dir` must refer to the active run '{run_id}'"
        )));
    }
    Ok(state_dir)
}

pub fn read_pipeline(state_dir: &Path) -> Result<Value, OrbitError> {
    Ok(read_state_file(state_dir)?.pipeline)
}

pub fn write_step_output(
    state_dir: &Path,
    step_index: u32,
    data: &Value,
) -> Result<(), OrbitError> {
    let incoming = data
        .as_object()
        .ok_or_else(|| OrbitError::InvalidInput("step output must be a JSON object".to_string()))?;
    let mut state = read_state_file(state_dir)?;
    match state.step_outputs.entry(step_index) {
        Entry::Occupied(mut entry) => {
            let mut merged = match entry.get() {
                Value::Object(existing) => existing.clone(),
                _ => serde_json::Map::new(),
            };
            for (key, value) in incoming {
                merged.insert(key.clone(), value.clone());
            }
            entry.insert(Value::Object(merged));
        }
        Entry::Vacant(entry) => {
            entry.insert(Value::Object(incoming.clone()));
        }
    }
    state.updated_at = Utc::now();
    write_state_file(state_dir, &state)
}

fn read_state_file(state_dir: &Path) -> Result<PipelineState, OrbitError> {
    let state_path = state_path(state_dir);
    let raw = fs::read_to_string(&state_path).map_err(|error| {
        OrbitError::Io(format!(
            "failed to read state.json '{}': {error}",
            state_path.display()
        ))
    })?;
    serde_json::from_str(&raw).map_err(|error| {
        OrbitError::Store(format!(
            "invalid state.json '{}': {error}",
            state_path.display()
        ))
    })
}

fn write_state_file(state_dir: &Path, state: &PipelineState) -> Result<(), OrbitError> {
    let content = serde_json::to_string_pretty(state)
        .map_err(|error| OrbitError::Store(error.to_string()))?;
    write_atomic(&state_path(state_dir), &content).map_err(Into::into)
}

fn state_path(state_dir: &Path) -> PathBuf {
    state_dir.join("state.json")
}

fn validate_run_id(run_id: &str) -> Result<(), OrbitError> {
    validate_path_stem(run_id, "job run")
}

fn canonical_runs_root(orbit_root: &Path) -> Result<Option<PathBuf>, OrbitError> {
    let runs_root = orbit_root.join("state").join("job-runs");
    if !runs_root.exists() {
        return Ok(None);
    }

    let orbit_root = canonicalize_dir(orbit_root, "orbit root")?;
    let runs_root = canonicalize_dir(&runs_root, "job-runs root")?;
    if runs_root.starts_with(&orbit_root) {
        return Ok(Some(runs_root));
    }
    Err(OrbitError::InvalidInput(format!(
        "resolved job-runs root '{}' is outside '{}'",
        runs_root.display(),
        orbit_root.display()
    )))
}

fn canonicalize_dir(path: &Path, label: &str) -> Result<PathBuf, OrbitError> {
    let canonical = path.canonicalize().map_err(|error| {
        OrbitError::InvalidInput(format!(
            "{label} '{}' must exist and resolve safely: {error}",
            path.display()
        ))
    })?;
    if !canonical.is_dir() {
        return Err(OrbitError::InvalidInput(format!(
            "{label} '{}' must be a directory",
            path.display()
        )));
    }
    Ok(canonical)
}

fn ensure_under_runs_root(path: &Path, runs_root: &Path) -> Result<(), OrbitError> {
    if path.starts_with(runs_root) {
        return Ok(());
    }
    Err(OrbitError::InvalidInput(format!(
        "resolved state directory '{}' is outside '{}'",
        path.display(),
        runs_root.display()
    )))
}

fn path_contains_parent_dir(path: &Path) -> bool {
    path.components()
        .any(|component| matches!(component, Component::ParentDir))
}
