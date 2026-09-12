use std::path::{Component, Path, PathBuf};

use orbit_common::OrbitError;

pub(crate) fn validate_path_stem(stem: &str, kind: &str) -> Result<(), OrbitError> {
    if is_safe_path_stem(stem) {
        return Ok(());
    }

    Err(OrbitError::InvalidInput(format!(
        "{kind} id must be a single path component without separators or traversal: {stem}"
    )))
}

/// Validate a persisted task-store partition id: the canonical `ws_<name>`
/// form a workspace registration supplies, or the legacy `<slug>-<hash>` form
/// the task registry mints for an unregistered checkout.
pub(crate) fn validate_partition_id(raw: &str) -> Result<String, OrbitError> {
    let trimmed = raw.trim();
    let logical = trimmed.strip_prefix("ws_").is_some_and(|name| {
        !name.is_empty()
            && name
                .bytes()
                .all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_'))
    });
    let legacy = trimmed.rsplit_once('-').is_some_and(|(slug, suffix)| {
        !slug.is_empty()
            && !slug.starts_with('-')
            && !slug.ends_with('-')
            && !slug.contains("--")
            && slug
                .bytes()
                .all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'-'))
            && suffix.len() == 6
            && suffix
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    });

    if logical || legacy {
        Ok(trimmed.to_string())
    } else {
        Err(OrbitError::InvalidInput(format!(
            "workspace_id '{trimmed}' must use canonical ws_<name> or legacy <slug>-<6char> form"
        )))
    }
}

fn is_safe_path_stem(stem: &str) -> bool {
    let mut components = Path::new(stem).components();
    matches!(
        (components.next(), components.next()),
        (Some(Component::Normal(part)), None) if part.to_str() == Some(stem)
    )
}

pub(crate) fn normalize_path(path: &Path) -> PathBuf {
    if let Ok(canonical) = path.canonicalize() {
        return canonical;
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}
