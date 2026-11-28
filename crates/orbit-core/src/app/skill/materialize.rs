use std::borrow::Cow;
use std::path::Path;

use orbit_common::OrbitError;

use crate::paths::ORBIT_ROOT_TOKEN;
use crate::unit::managed_asset::{
    ManagedAssetLayout, ManagedAssetReconciliation, reconcile_managed_assets,
};

use super::catalog::DEFAULT_SKILL_FILES;

/// Materialize the shipped skill trees under `skills_root`, recording the
/// digest Orbit wrote for each file so a skill (or a single reference file)
/// dropped from a later release can be retired by content provenance.
///
/// The digest covers the *rendered* document: `ORBIT_ROOT_TOKEN` resolves to
/// the absolute root before the write, so an unchanged release re-seeds as a
/// no-op on the same root.
// ADR-0366: skills are managed by relative path, so a single reference
// file can be retired independently of its SKILL.md.
pub(crate) fn seed_default_skills(
    skills_root: &Path,
    orbit_root: &Path,
    overwrite: bool,
) -> Result<ManagedAssetReconciliation, OrbitError> {
    reconcile_managed_assets(
        skills_root,
        "skill",
        ManagedAssetLayout::RelativePath,
        DEFAULT_SKILL_FILES,
        overwrite,
        |_, content| {
            Ok(Cow::Owned(inject_skill_template_tokens(
                content, orbit_root,
            )))
        },
    )
}

/// A legacy workspace skill may be removed only when every entry in its tree
/// is a shipped file with unchanged bytes. Missing shipped references are
/// allowed because older releases seeded smaller trees; unknown entries and
/// symlinks are treated as operator-owned content.
pub(crate) fn is_default_skill_tree_for_root(
    skill_id: &str,
    skill_dir: &Path,
    orbit_root: &Path,
) -> Result<bool, OrbitError> {
    let prefix = format!("{skill_id}/");
    let expected: Vec<_> = DEFAULT_SKILL_FILES
        .iter()
        .filter_map(|(path, content)| path.strip_prefix(&prefix).map(|path| (path, *content)))
        .collect();
    if expected.is_empty() {
        return Ok(false);
    }
    let metadata = match std::fs::symlink_metadata(skill_dir) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(OrbitError::Io(error.to_string())),
    };
    if !metadata.file_type().is_dir() {
        return Ok(false);
    }

    let mut found_router = false;
    let mut pending = vec![skill_dir.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).map_err(|e| OrbitError::Io(e.to_string()))? {
            let entry = entry.map_err(|e| OrbitError::Io(e.to_string()))?;
            let path = entry.path();
            let relative = path
                .strip_prefix(skill_dir)
                .map_err(|e| OrbitError::Io(e.to_string()))?;
            let file_type = entry
                .file_type()
                .map_err(|e| OrbitError::Io(e.to_string()))?;
            if file_type.is_dir() {
                if !expected
                    .iter()
                    .any(|(name, _)| Path::new(name).starts_with(relative))
                {
                    return Ok(false);
                }
                pending.push(path);
            } else if file_type.is_file() {
                let Some((_, content)) = expected
                    .iter()
                    .find(|(name, _)| Path::new(name) == relative)
                else {
                    return Ok(false);
                };
                let existing = std::fs::read(&path).map_err(|e| OrbitError::Io(e.to_string()))?;
                if existing != inject_skill_template_tokens(content, orbit_root).as_bytes() {
                    return Ok(false);
                }
                found_router |= relative == Path::new("SKILL.md");
            } else {
                return Ok(false);
            }
        }
    }
    Ok(found_router)
}

pub(crate) fn inject_skill_template_tokens(raw: &str, orbit_root: &Path) -> String {
    let orbit_root_value = orbit_root.to_string_lossy();
    raw.replace(ORBIT_ROOT_TOKEN, orbit_root_value.as_ref())
}
