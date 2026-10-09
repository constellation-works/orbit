//! Preserving modified retired assets and reporting ambiguous legacy files.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self};
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;

use super::ManagedAssetLayout;

pub(crate) fn retired_preservation_path(
    active_dir: &Path,
    asset_kind: &str,
    layout: ManagedAssetLayout,
    name: &str,
) -> PathBuf {
    let (base, backup_relative) = retired_preservation_root(active_dir, asset_kind);
    base.join(backup_relative).join(layout.relative_path(name))
}

fn retired_preservation_root(active_dir: &Path, asset_kind: &str) -> (PathBuf, PathBuf) {
    (
        active_dir.parent().unwrap_or(active_dir).to_path_buf(),
        Path::new(".retired-managed").join(managed_asset_kind_directory(asset_kind)),
    )
}

/// The first existing directory component of a retired asset's preservation
/// destination that is a link or not a directory, if any. Components below
/// the catalog's parent are inspected without following links, so moving a
/// modified asset aside can never place it outside that tree.
pub(super) fn unsafe_preservation_component(
    active_dir: &Path,
    asset_kind: &str,
    layout: ManagedAssetLayout,
    name: &str,
) -> Result<Option<PathBuf>, OrbitError> {
    let (base, backup_relative) = retired_preservation_root(active_dir, asset_kind);
    let relative = backup_relative.join(layout.relative_path(name));
    let Some(parent) = relative.parent() else {
        return Ok(None);
    };
    let mut target = base;
    for component in parent.components() {
        target.push(component);
        match fs::symlink_metadata(&target) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Ok(Some(target));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(OrbitError::io_with_context(
                    &error,
                    format!(
                        "inspect retired managed asset backup '{}': {error}",
                        target.display()
                    ),
                ));
            }
        }
    }
    Ok(None)
}

pub(in crate::application) fn preserve_modified_retired_asset(
    active_dir: &Path,
    asset_kind: &str,
    layout: ManagedAssetLayout,
    name: &str,
    source: &Path,
) -> Result<PathBuf, OrbitError> {
    if let Some(component) = unsafe_preservation_component(active_dir, asset_kind, layout, name)? {
        return Err(OrbitError::InvalidInput(format!(
            "refusing to preserve retired managed {asset_kind} `{name}` through '{}': it is linked or is not a directory",
            component.display()
        )));
    }
    let (base, backup_relative) = retired_preservation_root(active_dir, asset_kind);
    let backup_root = base.join(backup_relative);
    let relative = layout.relative_path(name);

    let mut suffix = 0usize;
    loop {
        // Disambiguate on the file stem so a preserved `SKILL.md` keeps its
        // extension and stays readable in place.
        let destination = if suffix == 0 {
            backup_root.join(&relative)
        } else {
            let file_name = relative
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| {
                    OrbitError::InvalidInput(format!(
                        "retired managed {asset_kind} `{name}` has no file name to preserve"
                    ))
                })?;
            let (stem, extension) = file_name
                .rsplit_once('.')
                .map_or((file_name, String::new()), |(stem, extension)| {
                    (stem, format!(".{extension}"))
                });
            backup_root
                .join(&relative)
                .with_file_name(format!("{stem}.{suffix}{extension}"))
        };
        // `symlink_metadata` so a dangling link also counts as occupied.
        if fs::symlink_metadata(&destination).is_ok() {
            suffix += 1;
            continue;
        }
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                OrbitError::io_with_context(
                    &error,
                    format!(
                        "create retired managed asset backup '{}': {error}",
                        parent.display()
                    ),
                )
            })?;
        }
        fs::rename(source, &destination).map_err(|error| {
            OrbitError::io_with_context(
                &error,
                format!(
                    "preserve modified retired {asset_kind} '{}' as '{}': {error}",
                    source.display(),
                    destination.display()
                ),
            )
        })?;
        return Ok(destination);
    }
}

fn managed_asset_kind_directory(asset_kind: &str) -> String {
    match asset_kind {
        "activity" => "activities".to_string(),
        "job" => "jobs".to_string(),
        other => format!("{other}s"),
    }
}

pub(super) fn ambiguous_legacy_yaml_files(
    dir: &Path,
    managed_assets: &BTreeMap<String, String>,
) -> Result<Vec<PathBuf>, OrbitError> {
    let mut ambiguous = Vec::new();
    let entries = fs::read_dir(dir).map_err(|error| {
        OrbitError::io_with_context(
            &error,
            format!(
                "inspect legacy managed asset directory '{}': {error}",
                dir.display()
            ),
        )
    })?;
    for entry in entries {
        let path = entry.map_err(OrbitError::from)?.path();
        let is_yaml = path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension == "yaml" || extension == "yml");
        if !is_yaml {
            continue;
        }
        let stem = path.file_stem().and_then(|stem| stem.to_str());
        if stem.is_none_or(|stem| !managed_assets.contains_key(stem)) {
            ambiguous.push(path);
        }
    }
    ambiguous.sort();
    Ok(ambiguous)
}
