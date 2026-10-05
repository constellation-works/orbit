//! Pending-migration listing, upgrade application and forward-compatible reporting.

use std::path::Path;

use orbit_common::OrbitError;

use super::marker::{
    read_compat_record, read_marker, upgrade_lock_path, write_compat_record, write_marker,
};
use super::registry::{LAYOUT_MIGRATIONS, LayoutMigration, SUPPORTED_LAYOUT_VERSION};
use crate::contracts::{
    CompatibilityRefusal, ForwardCompatibleOpen, StateComponent, evaluate_newer_state,
};

/// Registry metadata for one layout migration, as surfaced to `orbit
/// migrate` (pending listings and applied reports).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutMigrationInfo {
    pub version: u32,
    pub name: String,
    pub description: String,
}

impl LayoutMigrationInfo {
    fn from_entry(entry: &LayoutMigration) -> Self {
        Self {
            version: entry.version,
            name: entry.name.to_string(),
            description: entry.description.to_string(),
        }
    }
}

/// Outcome of a workspace-layout pre-flight/upgrade.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LayoutUpgradeReport {
    /// Marker version before the upgrade (0 = pre-versioning workspace).
    pub from_version: u32,
    /// Marker version after the upgrade.
    pub to_version: u32,
    /// Migrations applied by this call, in order. Empty when the workspace
    /// was already current.
    pub applied: Vec<LayoutMigrationInfo>,
    /// Set when the workspace layout is newer than this binary supports but
    /// by no breaking migration (ORB-12434). Nothing was applied and the
    /// marker was not rewritten; the report says whether this binary may
    /// keep writing (additive only) or only read (read-compatible).
    pub forward_compatible: Option<ForwardCompatibleOpen>,
}

/// Current layout version recorded in the workspace marker; 0 when no marker
/// exists (fresh or pre-versioning workspace).
pub fn current_layout_version(orbit_dir: &Path) -> Result<u32, OrbitError> {
    read_marker(orbit_dir)
}

/// Registry migrations newer than the workspace's recorded layout version,
/// in apply order. Empty when the workspace is current (or newer than this
/// binary — compare versions to distinguish; see [`SUPPORTED_LAYOUT_VERSION`]).
pub fn pending_layout_migrations(orbit_dir: &Path) -> Result<Vec<LayoutMigrationInfo>, OrbitError> {
    pending_with(orbit_dir, LAYOUT_MIGRATIONS)
}

/// Bring the workspace `.orbit` layout up to the newest supported version,
/// applying any pending registry migrations and advancing the marker after
/// each one. Refuses a workspace whose recorded layout version is newer than
/// this binary supports. The up-to-date fast path costs one marker read.
pub fn upgrade_workspace_layout(orbit_dir: &Path) -> Result<LayoutUpgradeReport, OrbitError> {
    upgrade_with(orbit_dir, LAYOUT_MIGRATIONS)
}

/// Whether a workspace whose layout is *newer* than this binary supports may
/// still be opened read-only (ORB-12434).
///
/// `Ok(None)` covers both "not newer" and "newer in a way this binary must
/// refuse" — read-only inspection surfaces such as `orbit migrate --dry-run`
/// compare versions themselves and only need to know whether the newer
/// workspace is usable. [`upgrade_workspace_layout`] carries the refusal.
pub fn layout_forward_compatible_open(
    orbit_dir: &Path,
) -> Result<Option<ForwardCompatibleOpen>, OrbitError> {
    let current = read_marker(orbit_dir)?;
    if current <= SUPPORTED_LAYOUT_VERSION {
        return Ok(None);
    }
    Ok(evaluate_marker(orbit_dir, current, SUPPORTED_LAYOUT_VERSION)?.ok())
}

pub(crate) fn pending_with(
    orbit_dir: &Path,
    migrations: &[LayoutMigration],
) -> Result<Vec<LayoutMigrationInfo>, OrbitError> {
    validate_registry(migrations)?;
    let current = read_marker(orbit_dir)?;
    Ok(migrations
        .iter()
        .filter(|m| m.version > current)
        .map(LayoutMigrationInfo::from_entry)
        .collect())
}

pub(crate) fn upgrade_with(
    orbit_dir: &Path,
    migrations: &[LayoutMigration],
) -> Result<LayoutUpgradeReport, OrbitError> {
    validate_registry(migrations)?;
    let supported = migrations.last().map(|m| m.version).unwrap_or(0);

    let current = read_marker(orbit_dir)?;
    if let Some(report) = forward_compatible_report(orbit_dir, current, supported)? {
        return Ok(report);
    }
    if current == supported {
        return Ok(LayoutUpgradeReport {
            from_version: current,
            to_version: current,
            applied: Vec::new(),
            forward_compatible: None,
        });
    }

    // Slow path: serialize concurrent upgraders, then re-check the marker —
    // another process may have finished the upgrade while we waited.
    let _guard =
        crate::fs::lock::acquire_exclusive(&upgrade_lock_path(orbit_dir), "layout upgrade")?;
    let from_version = read_marker(orbit_dir)?;
    if let Some(report) = forward_compatible_report(orbit_dir, from_version, supported)? {
        return Ok(report);
    }

    let mut applied = Vec::new();
    let mut version = from_version;
    for migration in migrations.iter().filter(|m| m.version > from_version) {
        (migration.apply)(orbit_dir).map_err(|error| {
            OrbitError::Migration(format!(
                "layout migration v{} ({}) failed for '{}': {error}",
                migration.version,
                migration.name,
                orbit_dir.display()
            ))
        })?;
        write_marker(orbit_dir, migration.version)?;
        write_compat_record(orbit_dir, migrations, migration.version)?;
        version = migration.version;
        applied.push(LayoutMigrationInfo::from_entry(migration));
        orbit_common::tracing::info!(
            target: "orbit.store.layout",
            version = migration.version,
            name = migration.name,
            orbit_dir = %orbit_dir.display(),
            "applied workspace layout migration",
        );
    }

    Ok(LayoutUpgradeReport {
        from_version,
        to_version: version,
        applied,
        forward_compatible: None,
    })
}

/// Decide a marker newer than `supported`: `Ok(None)` when it is not newer,
/// `Ok(Some(report))` when it is newer by no breaking migration (nothing
/// applied, marker untouched), and an error naming the first breaking
/// migration this binary lacks otherwise.
fn forward_compatible_report(
    orbit_dir: &Path,
    current: u32,
    supported: u32,
) -> Result<Option<LayoutUpgradeReport>, OrbitError> {
    if current <= supported {
        return Ok(None);
    }
    match evaluate_marker(orbit_dir, current, supported)? {
        Ok(forward) => {
            orbit_common::tracing::warn!(
                target: "orbit.store.layout",
                orbit_dir = %orbit_dir.display(),
                layout_version = current,
                supported_version = supported,
                "opening a newer workspace layout read-only; this binary applies no layout migration to it",
            );
            Ok(Some(LayoutUpgradeReport {
                from_version: current,
                to_version: current,
                applied: Vec::new(),
                forward_compatible: Some(forward),
            }))
        }
        Err(refusal) => Err(OrbitError::Migration(format!(
            "workspace '{}' has .orbit layout version {current}, newer than the newest version \
             this orbit binary supports ({supported}); {refusal}; upgrade orbit to open this \
             workspace",
            orbit_dir.display()
        ))),
    }
}

/// Read the companion compatibility record and decide whether this binary
/// may open the newer workspace. The layout is not write-gated, so a newer
/// read-compatible migration refuses like a breaking one. The outer error
/// covers only I/O on the record itself; an unusable record is an inner
/// [`CompatibilityRefusal`].
fn evaluate_marker(
    orbit_dir: &Path,
    current: u32,
    supported: u32,
) -> Result<Result<ForwardCompatibleOpen, CompatibilityRefusal>, OrbitError> {
    let record = match read_compat_record(orbit_dir)? {
        Ok(record) => record,
        Err(refusal) => return Ok(Err(refusal)),
    };
    Ok(evaluate_newer_state(
        StateComponent::WorkspaceLayout,
        current,
        supported,
        record,
        false,
    ))
}

fn validate_registry(migrations: &[LayoutMigration]) -> Result<(), OrbitError> {
    let mut previous = 0u32;
    for migration in migrations {
        if migration.version <= previous {
            return Err(OrbitError::Migration(format!(
                "layout migration registry is not strictly increasing: v{} ({}) follows v{previous}",
                migration.version, migration.name
            )));
        }
        previous = migration.version;
    }
    Ok(())
}
