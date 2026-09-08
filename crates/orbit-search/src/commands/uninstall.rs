use std::fs;

use orbit_common::OrbitError;
use serde::Serialize;

use crate::CompanionPaths;
use crate::commands::install::companion_integrity_path;
use crate::commands::{active_model, remove_file_if_exists};
use crate::{ModelSpec, default_model};

#[derive(Debug, Clone)]
pub struct SemanticUninstallParams {
    pub model: Option<String>,
    pub all: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SemanticUninstallResult {
    pub removed_companion: bool,
    pub removed_companion_integrity: bool,
    pub removed_temporary_companions: Vec<String>,
    pub removed_models: Vec<String>,
}

pub fn run(params: SemanticUninstallParams) -> Result<SemanticUninstallResult, OrbitError> {
    let paths = CompanionPaths::default_under_home()?;
    run_with_paths(&paths, params)
}

pub(crate) fn run_with_paths(
    paths: &CompanionPaths,
    params: SemanticUninstallParams,
) -> Result<SemanticUninstallResult, OrbitError> {
    if params.all {
        let companion_path = paths.companion_path();
        let removed_companion = remove_file_if_exists(&companion_path)?;
        let removed_companion_integrity = companion_integrity_path(&companion_path)
            .map(|path| remove_file_if_exists(&path))
            .transpose()?
            .unwrap_or(false);
        let removed_temporary_companions =
            remove_temporary_companions(&paths.bin_dir, &companion_path)?;
        let mut removed_models = Vec::new();
        if paths.models_dir.exists() {
            for entry in fs::read_dir(&paths.models_dir)
                .map_err(|error| OrbitError::Io(error.to_string()))?
            {
                let entry = entry.map_err(|error| OrbitError::Io(error.to_string()))?;
                if entry.path().is_dir() {
                    removed_models.push(entry.file_name().to_string_lossy().to_string());
                }
            }
            fs::remove_dir_all(&paths.models_dir)
                .map_err(|error| OrbitError::Io(error.to_string()))?;
        }
        let _ = remove_file_if_exists(&paths.active_model_path)?;
        remove_empty_directory(&paths.bin_dir)?;
        remove_empty_directory(&paths.root)?;
        return Ok(SemanticUninstallResult {
            removed_companion,
            removed_companion_integrity,
            removed_temporary_companions,
            removed_models,
        });
    }

    let model = match params.model {
        Some(model) => ModelSpec::parse(&model)?.alias.to_string(),
        None => active_model(paths).unwrap_or_else(|| default_model().alias.to_string()),
    };
    let model_dir = paths.model_dir(&model);
    let removed = if model_dir.exists() {
        fs::remove_dir_all(&model_dir).map_err(|error| OrbitError::Io(error.to_string()))?;
        true
    } else {
        false
    };
    if active_model(paths).as_deref() == Some(model.as_str()) {
        let _ = remove_file_if_exists(&paths.active_model_path)?;
    }

    Ok(SemanticUninstallResult {
        removed_companion: false,
        removed_companion_integrity: false,
        removed_temporary_companions: Vec::new(),
        removed_models: if removed { vec![model] } else { Vec::new() },
    })
}

fn remove_temporary_companions(
    bin_dir: &std::path::Path,
    companion_path: &std::path::Path,
) -> Result<Vec<String>, OrbitError> {
    let Some(companion_name) = companion_path.file_name().and_then(|name| name.to_str()) else {
        return Ok(Vec::new());
    };
    let temporary_prefix = format!(".{companion_name}.tmp-");
    let mut removed = Vec::new();

    if !bin_dir.exists() {
        return Ok(removed);
    }

    for entry in fs::read_dir(bin_dir).map_err(|error| OrbitError::Io(error.to_string()))? {
        let entry = entry.map_err(|error| OrbitError::Io(error.to_string()))?;
        let name = entry.file_name();
        if name.to_string_lossy().starts_with(&temporary_prefix) {
            fs::remove_file(entry.path()).map_err(|error| OrbitError::Io(error.to_string()))?;
            removed.push(name.to_string_lossy().to_string());
        }
    }

    Ok(removed)
}

fn remove_empty_directory(path: &std::path::Path) -> Result<(), OrbitError> {
    if path.exists() {
        match fs::remove_dir(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::DirectoryNotEmpty => {}
            Err(error) => return Err(OrbitError::Io(error.to_string())),
        }
    }
    Ok(())
}
