use super::*;

/// Restore linked-checkout metadata hidden by the private `/tmp`. Resolve
/// pointers from disk rather than invoking Git with ambient Git overrides.
/// The caller emits these read-only mounts before policy and read masks.
pub(super) fn append_git_metadata_mounts(
    out: &mut Vec<String>,
    cwd: &Path,
    mount_sources: &mut Vec<Arc<File>>,
) -> Result<(), OrbitError> {
    for root in cwd.ancestors() {
        let pointer = root.join(".git");
        match std::fs::metadata(&pointer) {
            Ok(metadata) if metadata.is_dir() => return Ok(()),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(pointer_error(&pointer, error)),
        }
        let text = read_pointer(&pointer)?;
        let target = text
            .trim()
            .strip_prefix("gitdir:")
            .map(str::trim)
            .filter(|target| !target.is_empty())
            .ok_or_else(|| OrbitError::InvalidInput("invalid Git metadata pointer".to_string()))?;
        let git_dir = canonical_existing(&root.join(target), "Git directory")?;
        let common_pointer = git_dir.join("commondir");
        let common_dir = match std::fs::read_to_string(&common_pointer) {
            Ok(target) if !target.trim().is_empty() => {
                canonical_existing(&git_dir.join(target.trim()), "Git common directory")?
            }
            Ok(_) => {
                return Err(OrbitError::InvalidInput(
                    "empty Git commondir pointer".to_string(),
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => git_dir.clone(),
            Err(error) => return Err(pointer_error(&common_pointer, error)),
        };
        // Parent paths sort before their children. Keep separate metadata
        // roots too: Git's commondir need not contain the per-worktree gitdir.
        for path in BTreeSet::from([git_dir, common_dir]) {
            if !path.is_dir() || path == Path::new("/tmp") {
                return Err(OrbitError::PolicyDenied(format!(
                    "linux-bwrap refuses Git metadata mount `{}`: expected a directory narrower than host /tmp",
                    path.display()
                )));
            }
            if path.starts_with("/tmp") {
                let source =
                    Arc::new(File::open(&path).map_err(|error| pointer_error(&path, error))?);
                let source = prepare_mount_source(source)?;
                #[cfg(unix)]
                {
                    out.extend([
                        "--ro-bind-fd".to_string(),
                        source.as_raw_fd().to_string(),
                        path.display().to_string(),
                    ]);
                }
                #[cfg(not(unix))]
                {
                    return Err(OrbitError::Execution(
                        "Linux sandbox Git metadata mounts require Unix file descriptors"
                            .to_string(),
                    ));
                }
                mount_sources.push(source);
            }
        }
        return Ok(());
    }
    Ok(())
}

fn read_pointer(path: &Path) -> Result<String, OrbitError> {
    std::fs::read_to_string(path).map_err(|error| pointer_error(path, error))
}

fn pointer_error(path: &Path, error: std::io::Error) -> OrbitError {
    OrbitError::Execution(format!(
        "read Linux sandbox Git metadata pointer `{}`: {error}",
        path.display()
    ))
}
