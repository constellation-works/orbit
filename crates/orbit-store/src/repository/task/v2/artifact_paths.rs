use super::*;

pub(super) fn normalize_v2_artifact_path(raw: &str) -> Result<String, OrbitError> {
    let mut trimmed = raw.trim();
    while let Some(rest) = trimmed.strip_prefix("./") {
        trimmed = rest;
    }
    validate_relative_artifact_path(trimmed)?;
    let mut parts = Vec::new();
    for component in Path::new(trimmed).components() {
        match component {
            Component::Normal(part) => {
                let part = part.to_str().ok_or_else(|| {
                    OrbitError::InvalidInput(format!(
                        "artifact path '{trimmed}' must be valid UTF-8"
                    ))
                })?;
                parts.push(part.to_string());
            }
            _ => {
                return Err(OrbitError::InvalidInput(format!(
                    "artifact path '{trimmed}' must be canonical"
                )));
            }
        }
    }
    Ok(parts.join("/"))
}

pub(super) fn resolve_v2_artifact_file_path(
    bundle_dir: &Path,
    blob: &str,
) -> Result<Option<PathBuf>, OrbitError> {
    let artifacts_dir = bundle_dir.join(TASK_ARTIFACTS_DIR_NAME);
    let artifacts_root = match fs::canonicalize(&artifacts_dir) {
        Ok(path) => path,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(OrbitError::Io(err.to_string())),
    };
    let artifact_file = match fs::canonicalize(artifacts_dir.join(blob)) {
        Ok(path) => path,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(OrbitError::Io(err.to_string())),
    };
    if !artifact_file.starts_with(&artifacts_root) {
        return Err(OrbitError::InvalidInput(format!(
            "artifact blob '{blob}' resolves outside the task artifact directory"
        )));
    }
    Ok(Some(artifact_file))
}
