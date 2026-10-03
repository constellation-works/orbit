//! Authority-root validation and containment of generation record paths.

use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};

use super::refusal::refusal;
use crate::OrbitError;

pub(super) const ADMISSION_LOCK: &str = ".generation-admission.lock";
pub(super) const GENERATION_LOCK: &str = ".generation.lock";
pub(super) const COMPAT_RECORD: &str = ".generation-compat.json";
pub(super) const IMAGE_DIGEST_CACHE: &str = ".generation-image-digest.json";
pub(super) const CLOCK_HOLD: &str = ".generation-clock-hold.json";

fn generation_record_name(name: &str) -> Result<&'static str, OrbitError> {
    match name {
        ADMISSION_LOCK => Ok(ADMISSION_LOCK),
        GENERATION_LOCK => Ok(GENERATION_LOCK),
        COMPAT_RECORD => Ok(COMPAT_RECORD),
        IMAGE_DIGEST_CACHE => Ok(IMAGE_DIGEST_CACHE),
        _ => Err(refusal("invalid generation record name")),
    }
}

/// CodeQL `rust/path-injection` treats `Path::starts_with` as a SafeAccessCheck
/// on the receiver. Call this after reconstructing a path so `is_dir` / open
/// sinks only see a prefix-checked value.
fn generation_path_is_contained(path: &Path, base: &Path) -> bool {
    path.starts_with(base)
}

fn generation_leaf_name(root: &Path) -> Result<&OsStr, OrbitError> {
    let Some(name) = root.file_name() else {
        return Err(refusal("generation root must not be empty"));
    };
    if name == "." || name == ".." {
        return Err(refusal("generation root escapes its start"));
    }
    Ok(name)
}

fn contained_under_parent(parent: &Path, name: &OsStr) -> Result<PathBuf, OrbitError> {
    let contained = parent.join(name);
    if !generation_path_is_contained(&contained, parent) {
        return Err(refusal("generation root escapes its parent"));
    }
    Ok(contained)
}

/// Resolve the authority root before any generation lock is created or opened.
///
/// Callers pass `~/.orbit` or a test directory; both are untrusted path values.
/// An existing root is canonicalized so aliases collapse to one directory, then
/// reconstructed under its canonical parent so later filesystem sinks only see
/// a prefix-checked path. A missing root whose parent exists is joined onto
/// that canonical parent. A missing parent is reconstructed from components so
/// `..` cannot walk outside the starting location before `create_dir_all`.
pub(crate) fn validated_generation_root(root: &Path) -> Result<PathBuf, OrbitError> {
    if root.as_os_str().is_empty() {
        return Err(refusal("generation root must not be empty"));
    }
    match root.canonicalize() {
        Ok(canonical) => existing_generation_root(canonical),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => missing_generation_root(root),
        Err(error) => Err(refusal(error)),
    }
}

fn existing_generation_root(canonical: PathBuf) -> Result<PathBuf, OrbitError> {
    let name = generation_leaf_name(&canonical)?;
    let parent = canonical
        .parent()
        .ok_or_else(|| refusal("generation root must be a directory"))?;
    let contained = parent.join(name);
    if !contained.starts_with(parent) {
        return Err(refusal("generation root escapes its parent"));
    }
    if !contained.is_dir() {
        return Err(refusal("generation root must be a directory"));
    }
    Ok(contained)
}

fn missing_generation_root(root: &Path) -> Result<PathBuf, OrbitError> {
    let name = generation_leaf_name(root)?;
    let Some(parent) = root.parent().filter(|path| !path.as_os_str().is_empty()) else {
        return normalize_missing_generation_root(root);
    };
    match parent.canonicalize() {
        Ok(canonical_parent) => contained_under_parent(&canonical_parent, name),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            normalize_missing_generation_root(root)
        }
        Err(error) => Err(refusal(error)),
    }
}

fn normalize_missing_generation_root(root: &Path) -> Result<PathBuf, OrbitError> {
    let mut normalized = PathBuf::new();
    for component in root.components() {
        match component {
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(
                    normalized.components().next_back(),
                    Some(Component::Normal(_))
                ) {
                    normalized.pop();
                } else {
                    return Err(refusal("generation root escapes its start"));
                }
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        return Err(refusal("generation root must not be empty"));
    }
    match (normalized.parent(), normalized.file_name()) {
        (Some(parent), Some(name)) if !parent.as_os_str().is_empty() => {
            contained_under_parent(parent, name)
        }
        _ => Ok(normalized),
    }
}

/// The directory whose generation records identify one authority.
///
/// Two spellings of the same authority — `~/.orbit` and a `--root` naming it
/// through a symlink, say — collapse to one value here. A caller that admits
/// against several roots at once must compare them this way before locking:
/// flock treats a second open of the same file as a foreign holder, so locking
/// one authority twice would refuse the update against itself.
pub fn authority_root(root: &Path) -> Result<PathBuf, OrbitError> {
    validated_generation_root(root)
}

/// Join an allow-listed generation record name onto a validated root.
///
/// The original caller string never reaches `Path::join`; only the matching
/// static name does. Containment is re-checked with `starts_with` after the
/// join so the open and `create_dir_all` sinks receive a reconstructed path
/// rather than the user-provided values.
pub(super) fn validated_generation_record_path(
    root: &Path,
    name: &str,
) -> Result<PathBuf, OrbitError> {
    let root = validated_generation_root(root)?;
    let name = generation_record_name(name)?;
    let path = root.join(name);
    if !generation_path_is_contained(&path, &root) {
        return Err(refusal("generation record path escapes the root"));
    }
    if path.parent() != Some(root.as_path()) {
        return Err(refusal("generation record path escapes the root"));
    }
    Ok(path)
}
