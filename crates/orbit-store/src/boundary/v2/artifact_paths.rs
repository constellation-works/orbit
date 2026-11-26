use super::*;

pub(crate) fn normalize_v2_artifact_path(raw: &str) -> Result<String, OrbitError> {
    Ok(orbit_types::task::canonical_artifact_path(raw)?)
}

pub(crate) fn resolve_v2_artifact_file_path(
    bundle_dir: &Path,
    blob: &str,
) -> Result<Option<PathBuf>, OrbitError> {
    crate::fs::path_safety::resolve_contained_artifact_blob(
        &bundle_dir.join(TASK_ARTIFACTS_DIR_NAME),
        blob,
    )
}
