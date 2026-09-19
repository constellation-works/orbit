use std::path::Path;

use orbit_types::task::{TaskArtifact, media_type_for_artifact_path};

use crate::error::OrbitError;

pub fn task_artifact_from_source_file(
    source_path: &Path,
    artifact_path: Option<&str>,
) -> Result<TaskArtifact, OrbitError> {
    let metadata = std::fs::metadata(source_path).map_err(|error| {
        OrbitError::InvalidInput(format!(
            "cannot read task artifact source '{}': {error}",
            source_path.display()
        ))
    })?;
    if !metadata.is_file() {
        return Err(OrbitError::InvalidInput(format!(
            "task artifact source '{}' must be a file",
            source_path.display()
        )));
    }
    let path = match artifact_path {
        Some(path) => {
            let trimmed = path.trim();
            if trimmed.is_empty() {
                return Err(OrbitError::InvalidInput(
                    "task artifact path must not be empty".to_string(),
                ));
            }
            trimmed.to_string()
        }
        None => infer_artifact_path_from_source(source_path)?,
    };
    let content = std::fs::read(source_path).map_err(|error| {
        OrbitError::InvalidInput(format!(
            "cannot read task artifact source '{}': {error}",
            source_path.display()
        ))
    })?;
    Ok(TaskArtifact {
        media_type: media_type_for_artifact_path(&path).to_string(),
        path,
        content,
        created_by: None,
    })
}

fn infer_artifact_path_from_source(source_path: &Path) -> Result<String, OrbitError> {
    let file_name = source_path
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| {
            OrbitError::InvalidInput(format!(
                "cannot infer task artifact path from source '{}'; pass --path",
                source_path.display()
            ))
        })?;
    Ok(file_name.to_string())
}
