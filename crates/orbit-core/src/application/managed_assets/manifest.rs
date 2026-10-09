//! The per-directory provenance manifest, and path confinement for managed assets.

use std::fs;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::fs::io::{atomic_write_text, is_readonly_or_access_error};

use super::{
    MANAGED_ASSET_MANIFEST_SCHEMA_VERSION, ManagedAssetLayout, ManagedAssetManifest,
    ROUTINE_MANAGED_ASSET_MANIFEST_SCHEMA_VERSION,
};

/// Persist one managed-asset manifest. Callers compare against the previous
/// manifest first so a steady-state bootstrap performs no write at all.
///
/// Explicit repair paths (`orbit doctor --fix-stale-artifacts`) use this
/// fail-closed helper. Reconciliation uses [`record_managed_manifest_write`]
/// so a read-only global root does not fail a later read-only command.
pub(in crate::application) fn write_managed_asset_manifest(
    manifest_path: &Path,
    manifest: &ManagedAssetManifest,
) -> Result<(), OrbitError> {
    let encoded = encode_managed_asset_manifest(manifest)?;
    atomic_write_text(manifest_path, &encoded).map_err(|error| {
        managed_asset_manifest_io_error(manifest_path, &manifest.asset_kind, error)
    })
}

pub(in crate::application) fn encode_managed_asset_manifest(
    manifest: &ManagedAssetManifest,
) -> Result<String, OrbitError> {
    let asset_kind = &manifest.asset_kind;
    let mut encoded = serde_json::to_string_pretty(manifest).map_err(|error| {
        OrbitError::Store(format!(
            "serialize managed {asset_kind} asset manifest: {error}"
        ))
    })?;
    encoded.push('\n');
    Ok(encoded)
}

/// Read-only / permission denials on a needed manifest write are a
/// deployment shape (immutable global root, sandboxed runner), not a
/// reason to refuse a later read-only command.
pub(crate) fn managed_manifest_write_is_skippable(error: &io::Error) -> bool {
    is_readonly_or_access_error(error)
}

fn managed_asset_manifest_io_error(
    manifest_path: &Path,
    asset_kind: &str,
    error: io::Error,
) -> OrbitError {
    OrbitError::io_with_context(
        &error,
        format!(
            "write managed {asset_kind} asset manifest '{}': {error}",
            manifest_path.display()
        ),
    )
}

/// Record a needed manifest write, warning (instead of failing closed) when
/// the destination is EROFS/EACCES. Other I/O failures stay fatal. Returns
/// whether the manifest was persisted.
pub(crate) fn record_managed_manifest_write(
    manifest_path: &Path,
    asset_kind: &str,
    write_result: Result<(), io::Error>,
    warnings: &mut Vec<String>,
) -> Result<bool, OrbitError> {
    match write_result {
        Ok(()) => Ok(true),
        Err(error) if managed_manifest_write_is_skippable(&error) => {
            warnings.push(format!(
                "could not write managed {asset_kind} asset manifest '{}': {error}; continuing without updating it",
                manifest_path.display()
            ));
            Ok(false)
        }
        Err(error) => Err(managed_asset_manifest_io_error(
            manifest_path,
            asset_kind,
            error,
        )),
    }
}

pub(in crate::application) fn load_managed_asset_manifest(
    path: &Path,
    expected_kind: &str,
    layout: ManagedAssetLayout,
) -> Result<Option<ManagedAssetManifest>, OrbitError> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(path).map_err(|error| {
        OrbitError::io_with_context(
            &error,
            format!("read managed asset manifest '{}': {error}", path.display()),
        )
    })?;
    let manifest: ManagedAssetManifest = serde_json::from_str(&raw).map_err(|error| {
        OrbitError::InvalidInput(format!(
            "managed asset manifest '{}' is invalid: {error}; repair it or move it aside only after reviewing the managed YAML files",
            path.display()
        ))
    })?;
    let supported_schema = manifest.schema_version == MANAGED_ASSET_MANIFEST_SCHEMA_VERSION
        || (expected_kind == "routine"
            && manifest.schema_version == ROUTINE_MANAGED_ASSET_MANIFEST_SCHEMA_VERSION);
    if !supported_schema {
        return Err(OrbitError::InvalidInput(format!(
            "managed asset manifest '{}' uses unsupported schemaVersion {}; expected {}{}",
            path.display(),
            manifest.schema_version,
            MANAGED_ASSET_MANIFEST_SCHEMA_VERSION,
            if expected_kind == "routine" {
                format!(" or {ROUTINE_MANAGED_ASSET_MANIFEST_SCHEMA_VERSION}")
            } else {
                String::new()
            }
        )));
    }
    if manifest.asset_kind != expected_kind {
        return Err(OrbitError::InvalidInput(format!(
            "managed asset manifest '{}' is for `{}`, expected `{expected_kind}`",
            path.display(),
            manifest.asset_kind
        )));
    }
    for name in manifest.assets.keys() {
        validate_managed_asset_name(name, layout, "manifest asset")?;
    }
    Ok(Some(manifest))
}

/// Reject any manifest key that would not stay inside the managed directory.
///
/// A stem is a single path component of the safe charset. A relative path is a
/// `/`-separated sequence of such components: no absolute prefix, no `.`/`..`
/// component, and a restricted charset per component, so a manifest can never
/// steer a write or a removal outside the directory it manages.
pub(super) fn validate_managed_asset_name(
    name: &str,
    layout: ManagedAssetLayout,
    source: &str,
) -> Result<(), OrbitError> {
    let component_ok = |component: &str| {
        !component.is_empty()
            && component != "."
            && component != ".."
            && component.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-' || byte == b'.'
            })
    };
    let valid = match layout {
        // A stem is one component and never carries an extension separator.
        ManagedAssetLayout::YamlStem => {
            !name.is_empty()
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
        }
        ManagedAssetLayout::RelativePath => {
            !name.is_empty()
                && !name.starts_with('/')
                && !name.contains('\\')
                && name.split('/').all(component_ok)
        }
    };
    if !valid {
        return Err(OrbitError::InvalidInput(format!(
            "{source} name `{name}` is not a safe managed asset path"
        )));
    }
    Ok(())
}

/// Where one managed path resolves beneath its catalog directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ConfinedAssetPath {
    /// Every intermediate component is a real directory and the final one a
    /// regular file.
    File(PathBuf),
    /// A component does not exist; every component before it is a real
    /// directory, so creating the rest stays inside the catalog.
    Missing,
    /// The first component that is a symlink (dangling or not) or is not the
    /// expected directory or regular-file type.
    Unsafe(PathBuf),
}

/// Resolve `relative` beneath `dir` without following links, so reading,
/// writing, or removing the result cannot act on a target outside `dir`.
///
/// `dir` itself is the trusted catalog root. The relative path is re-checked
/// even when it came from a validated manifest key. Every managed-asset read,
/// write, retirement, and doctor repair goes through this one boundary.
pub(crate) fn resolve_confined_asset_path(
    dir: &Path,
    relative: &Path,
) -> Result<ConfinedAssetPath, OrbitError> {
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(OrbitError::InvalidInput(format!(
            "managed artifact path '{}' must remain relative to '{}'",
            relative.display(),
            dir.display()
        )));
    }
    let mut target = dir.to_path_buf();
    let mut components = relative.components().peekable();
    while let Some(component) = components.next() {
        target.push(component);
        let metadata = match fs::symlink_metadata(&target) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(ConfinedAssetPath::Missing);
            }
            Err(error) => {
                return Err(OrbitError::io_with_context(
                    &error,
                    format!("inspect managed artifact '{}': {error}", target.display()),
                ));
            }
        };
        let expected_type = if components.peek().is_some() {
            metadata.is_dir()
        } else {
            metadata.is_file()
        };
        if metadata.file_type().is_symlink() || !expected_type {
            return Ok(ConfinedAssetPath::Unsafe(target));
        }
    }
    Ok(ConfinedAssetPath::File(target))
}

/// Write one managed asset whose path [`resolve_confined_asset_path`] proved
/// confined. An existing file is replaced by rename, which swaps the directory
/// entry instead of writing through whatever it names; a new file is created
/// exclusively, so a link appearing at the final component fails the write
/// rather than redirecting it.
pub(super) fn write_confined_asset(
    path: &Path,
    content: &str,
    replace_existing: bool,
    asset_kind: &str,
) -> Result<(), OrbitError> {
    let written = if replace_existing {
        atomic_write_text(path, content)
    } else {
        create_new_text(path, content)
    };
    written.map_err(|error| {
        OrbitError::io_with_context(
            &error,
            format!("write managed {asset_kind} '{}': {error}", path.display()),
        )
    })
}

fn create_new_text(path: &Path, content: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        orbit_common::fs::io::create_private_dir_all(parent)?;
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    let result = file.write_all(content.as_bytes());
    drop(file);
    if let Err(write_error) = result {
        match fs::remove_file(path) {
            Ok(()) => return Err(write_error),
            Err(cleanup_error) if cleanup_error.kind() == io::ErrorKind::NotFound => {
                return Err(write_error);
            }
            Err(cleanup_error) => {
                return Err(io::Error::new(
                    write_error.kind(),
                    format!(
                        "{write_error}; failed to remove incomplete managed asset '{}': {cleanup_error}",
                        path.display()
                    ),
                ));
            }
        }
    }
    Ok(())
}
