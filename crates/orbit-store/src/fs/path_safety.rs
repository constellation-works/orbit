use std::fs;
use std::path::{Component, Path, PathBuf};

use orbit_common::OrbitError;

const ARTIFACT_BLOB_OUTSIDE_ROOT: &str = "resolves outside the task artifact directory";

/// Canonicalize `blob` under `artifacts_root` and refuse a target outside that root.
///
/// `Ok(None)` means the root or the blob is absent. Callers that must not drop
/// a manifest entry turn that into their own missing-blob error. A symlink
/// whose target leaves the root is [`OrbitError::InvalidInput`] carrying
/// `ARTIFACT_BLOB_OUTSIDE_ROOT`, the wording singular retrieval already
/// returns. Both sides are canonicalized so a bundle stored under a symlinked
/// ancestor still compares as one directory.
pub(crate) fn resolve_contained_artifact_blob(
    artifacts_root: &Path,
    blob: &str,
) -> Result<Option<PathBuf>, OrbitError> {
    if Path::new(blob).is_absolute() {
        return Err(artifact_blob_outside_root(blob));
    }
    let root = match fs::canonicalize(artifacts_root) {
        Ok(path) => path,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(OrbitError::Io(err.to_string())),
    };
    // Join the original root, matching the singular resolver: `Path::join`
    // would drop `root` for an absolute blob, which is already refused above.
    let resolved = match fs::canonicalize(artifacts_root.join(blob)) {
        Ok(path) => path,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(OrbitError::Io(err.to_string())),
    };
    if !resolved.starts_with(&root) {
        return Err(artifact_blob_outside_root(blob));
    }
    Ok(Some(resolved))
}

/// Read a manifest blob after `resolve_contained_artifact_blob`.
///
/// `Ok(None)` is absence, including a blob that disappears after it was
/// resolved. The returned bytes are the canonical file inside `artifacts_root`.
pub(crate) fn read_contained_artifact_blob(
    artifacts_root: &Path,
    blob: &str,
) -> Result<Option<Vec<u8>>, OrbitError> {
    let Some(path) = resolve_contained_artifact_blob(artifacts_root, blob)? else {
        return Ok(None);
    };
    match fs::read(&path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(OrbitError::Io(err.to_string())),
    }
}

pub(crate) fn artifact_blob_outside_root(blob: &str) -> OrbitError {
    OrbitError::InvalidInput(format!(
        "artifact blob '{blob}' {ARTIFACT_BLOB_OUTSIDE_ROOT}"
    ))
}

/// Whether `error` is the stable containment refusal from
/// `artifact_blob_outside_root`. Bundle reads must pass it through instead
/// of reclassifying it as corruption: singular HTTP retrieval maps this
/// `InvalidInput` to a client error whose body contains the wording.
pub(crate) fn is_artifact_containment_refusal(error: &OrbitError) -> bool {
    let OrbitError::InvalidInput(message) = error else {
        return false;
    };
    let Some((head, tail)) = message.rsplit_once("' ") else {
        return false;
    };
    head.starts_with("artifact blob '") && tail == ARTIFACT_BLOB_OUTSIDE_ROOT
}

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
