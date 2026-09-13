//! Versioned `.orbit/` workspace-layout migrations (ORB-10012, P3.4).
//!
//! The SQLite schema ledger ([`crate::driver::sqlite::migration`]) versions the store
//! *database*; this module versions everything else about a workspace's
//! on-disk `.orbit/` layout — directory structure, non-SQLite state files,
//! log/index locations. Layout migrations are an ordered, append-only
//! registry of `(version, name, description, apply)` entries mirroring the
//! schema ledger; the next breaking `.orbit/` change becomes a registry
//! entry instead of an undocumented break (see `RELEASING.md`).
//!
//! The current layout version is recorded in a plain-text marker file at
//! `<orbit_dir>/state/layout.version`. A marker file — not a `schema_meta`
//! row — because layout migrations may need to run *before* the store
//! database can open (a migration may move or restructure the database's own
//! location), and because reading one tiny file keeps the workspace-open
//! pre-flight cheap (no extra SQLite open on the hot path). A missing marker
//! means "pre-versioning workspace": all migrations run from the start, so
//! the v1 baseline (a no-op — version 1 *is* the current shape) adopts
//! existing workspaces exactly like the schema ledger's idempotent baseline
//! adopts legacy databases.
//!
//! Guarantees:
//! - **Auto-upgrade on open.** [`upgrade_workspace_layout`] runs as a
//!   pre-flight when a workspace opens (matching how the SQLite ledger
//!   auto-applies inside `Store::open`); `orbit migrate` is the explicit
//!   inspection/apply surface.
//! - **Forward-compatible open.** A marker newer than
//!   [`SUPPORTED_LAYOUT_VERSION`] is decided from the companion
//!   `state/layout.compat` record a newer binary leaves behind
//!   ([`crate::contracts::CompatibilityRecord`], ORB-12434): newer by
//!   additive migrations only opens read-only (nothing is applied and the
//!   marker is never rewritten); anything breaking — or a missing, stale, or
//!   unreadable record — still refuses with [`OrbitError::Migration`],
//!   naming the first breaking migration this binary lacks. The SQLite
//!   ledger guards its database the same way.
//! - **Crash tolerance.** Every migration MUST be idempotent (or stage via
//!   write-new-then-swap): the marker is advanced (atomic temp-file +
//!   rename) only *after* a migration's `apply` returns, so a crash in
//!   between re-runs that migration on the next open. The marker itself is
//!   gitignored runtime state — a fresh clone of a migrated workspace simply
//!   re-runs the idempotent migrations and re-stamps.
//! - **Single upgrader.** Pending migrations apply under an advisory file
//!   lock (`state/layout.lock`) with a re-check of the marker after
//!   acquisition, so concurrent opens do not interleave migrations. The
//!   up-to-date fast path never touches the lock.

use std::fs;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::fs::io::{atomic_write_text, create_private_dir_all};
use orbit_types::task::is_valid_orb_task_id;

use crate::contracts::{
    CompatibilityRecord, CompatibilityRefusal, ForwardCompatibleOpen, MigrationCompatibility,
    StateComponent, evaluate_newer_state,
};

/// Highest workspace-layout version this binary knows how to produce.
/// Bump together with a new [`LAYOUT_MIGRATIONS`] entry — never without one.
pub const SUPPORTED_LAYOUT_VERSION: u32 = 3;

/// Marker file recording the workspace's current layout version, relative to
/// the workspace `.orbit` directory. Lives under `state/` (gitignored
/// runtime state) so stamping a workspace never dirties repositories that
/// commit parts of `.orbit/`.
const MARKER_FILE: &str = "layout.version";

/// Companion forward-compatibility record, relative to `state/`. Written
/// beside the marker whenever a migration advances it, so a binary that is
/// older than the workspace can tell an additive layout bump from a breaking
/// one (ORB-12434). A separate file, not extra fields in the marker: shipped
/// binaries parse the whole marker as one integer, and a marker they cannot
/// parse would be a worse failure than the one this contract replaces.
const COMPAT_FILE: &str = "layout.compat";

/// Advisory lock serializing concurrent upgraders, relative to `state/`.
const UPGRADE_LOCK_FILE: &str = "layout.lock";

/// One entry in the layout-migration registry.
pub(crate) struct LayoutMigration {
    pub(crate) version: u32,
    pub(crate) name: &'static str,
    /// One-line human description surfaced by `orbit migrate --dry-run`.
    pub(crate) description: &'static str,
    /// What this migration means for a binary that does not have it. See
    /// [`MigrationCompatibility`]; declare `Breaking` when in doubt.
    pub(crate) compat: MigrationCompatibility,
    /// Applies the migration to the workspace `.orbit` directory. MUST be
    /// idempotent or staged (write-new-then-swap): a crash between `apply`
    /// and the marker write re-runs it on the next open. Directories the
    /// migration expects may be absent (fresh or partially-populated
    /// workspaces) — treat "nothing to do" as success.
    pub(crate) apply: fn(&Path) -> Result<(), OrbitError>,
}

/// Stable ordered registry of workspace-layout migrations. Append-only:
/// never renumber or edit an entry that has shipped. Every breaking
/// `.orbit/` layout change REQUIRES an entry here (see `RELEASING.md`).
pub(crate) const LAYOUT_MIGRATIONS: &[LayoutMigration] = &[
    LayoutMigration {
        version: 1,
        name: "baseline",
        description: "adopt the versioned .orbit/ layout (records the current shape; changes nothing)",
        // Records the shape older binaries already produce.
        compat: MigrationCompatibility::Additive,
        apply: apply_baseline_layout,
    },
    LayoutMigration {
        version: 2,
        name: "archive-friction-tasks",
        description: "rewrite affected task records from status 'friction' to 'archived', preserving the task and its event history",
        // Rewrites a removed status into one every binary understands; a
        // binary without this migration reads the result correctly.
        compat: MigrationCompatibility::Additive,
        apply: apply_archive_friction_tasks,
    },
    LayoutMigration {
        version: 3,
        name: "remove-task-checkout-projections",
        description: "remove verified legacy .orbit/tasks symlinks without following them or touching canonical task bundles",
        // ORB-11994/12078: binaries without this migration still read tasks
        // through the removed projections — and recreate them when they
        // write.
        compat: MigrationCompatibility::Breaking,
        apply: remove_legacy_task_projections,
    },
];

/// v1 baseline: the current `.orbit/` shape. Intentionally a no-op — running
/// it on any existing workspace changes nothing and then records version 1,
/// which is how pre-versioning workspaces adopt the marker.
fn apply_baseline_layout(_orbit_dir: &Path) -> Result<(), OrbitError> {
    Ok(())
}

/// v2: `TaskStatus::Friction` was removed in ORB-10202, so a persisted task
/// carrying that status no longer deserializes. Preserve the record but keep
/// it out of active work by mapping the removed status to `archived` in both
/// the task envelope and its event history.
///
/// Task projections may be symlinks into the canonical global bundle store.
/// `Path::is_dir` deliberately follows those direct projection entries; the
/// migration never recurses beyond one task bundle.
fn apply_archive_friction_tasks(orbit_dir: &Path) -> Result<(), OrbitError> {
    let tasks_dir = orbit_dir.join("tasks");
    let metadata = match fs::symlink_metadata(&tasks_dir) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(layout_io_error(
                "inspect projected task directory",
                &tasks_dir,
                error,
            ));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Ok(());
    }

    let entries = match fs::read_dir(&tasks_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(layout_io_error(
                "read projected task directory",
                &tasks_dir,
                error,
            ));
        }
    };

    for entry in entries {
        let entry = entry.map_err(|error| {
            layout_io_error("read projected task directory entry", &tasks_dir, error)
        })?;
        let bundle_dir = entry.path();
        let is_task_bundle = bundle_dir
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(is_valid_orb_task_id);
        if !is_task_bundle || !bundle_dir.is_dir() {
            continue;
        }

        migrate_event_statuses(&bundle_dir.join("events.jsonl"))?;
        migrate_envelope_status(&bundle_dir.join("task.yaml"))?;
    }
    Ok(())
}

/// v3: checkout-local task symlinks were a disposable convenience view over
/// canonical bundles. Remove only links whose names and targets match the
/// shipped `<global>/tasks/workspaces/<workspace>/<task-id>` shape. Anything
/// ambiguous is retained and diagnosed; in particular, never traverse a
/// symlink used as the `.orbit/tasks` parent.
fn remove_legacy_task_projections(orbit_dir: &Path) -> Result<(), OrbitError> {
    let tasks_dir = orbit_dir.join("tasks");
    let metadata = match fs::symlink_metadata(&tasks_dir) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(layout_io_error(
                "inspect legacy task directory",
                &tasks_dir,
                error,
            ));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        orbit_common::tracing::warn!(
            target: "orbit.store.layout",
            path = %tasks_dir.display(),
            "retained ambiguous legacy task path; expected a real directory",
        );
        return Ok(());
    }

    let entries = fs::read_dir(&tasks_dir)
        .map_err(|error| layout_io_error("read legacy task directory", &tasks_dir, error))?;
    for entry in entries {
        let entry = entry.map_err(|error| {
            layout_io_error("read legacy task directory entry", &tasks_dir, error)
        })?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| layout_io_error("inspect legacy task entry", &path, error))?;
        if !metadata.file_type().is_symlink() {
            continue;
        }
        let target = fs::read_link(&path)
            .map_err(|error| layout_io_error("read legacy task link", &path, error))?;
        if !is_owned_task_projection(&path, &target) {
            orbit_common::tracing::warn!(
                target: "orbit.store.layout",
                path = %path.display(),
                target = %target.display(),
                "retained unrecognized link in legacy task directory",
            );
            continue;
        }
        fs::remove_file(&path)
            .map_err(|error| layout_io_error("remove legacy task link", &path, error))?;
    }

    match fs::remove_dir(&tasks_dir) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::DirectoryNotEmpty => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(layout_io_error(
            "remove empty legacy task directory",
            &tasks_dir,
            error,
        )),
    }
}

fn is_owned_task_projection(link: &Path, target: &Path) -> bool {
    let Some(task_id) = link.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    if !is_valid_orb_task_id(task_id) || !target.is_absolute() {
        return false;
    }
    let Some(workspace_dir) = target.parent() else {
        return false;
    };
    let Some(workspaces_dir) = workspace_dir.parent() else {
        return false;
    };
    let Some(tasks_dir) = workspaces_dir.parent() else {
        return false;
    };

    target.file_name() == link.file_name()
        && workspace_dir.file_name().is_some()
        && workspaces_dir
            .file_name()
            .is_some_and(|name| name == "workspaces")
        && tasks_dir.file_name().is_some_and(|name| name == "tasks")
}

fn migrate_envelope_status(path: &Path) -> Result<(), OrbitError> {
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(layout_io_error("read task envelope", path, error)),
    };
    let mut value: serde_yaml::Value = serde_yaml::from_str(&raw).map_err(|error| {
        OrbitError::Migration(format!(
            "cannot parse task envelope '{}' while archiving removed friction status: {error}",
            path.display()
        ))
    })?;
    let Some(mapping) = value.as_mapping_mut() else {
        return Err(OrbitError::Migration(format!(
            "task envelope '{}' is not a YAML mapping",
            path.display()
        )));
    };
    if !replace_string_field(mapping, "status", "friction", "archived") {
        return Ok(());
    }

    let migrated = serde_yaml::to_string(&value).map_err(|error| {
        OrbitError::Migration(format!(
            "cannot serialize migrated task envelope '{}': {error}",
            path.display()
        ))
    })?;
    atomic_write_text(path, &migrated)
        .map_err(|error| layout_io_error("rewrite task envelope", path, error))
}

fn migrate_event_statuses(path: &Path) -> Result<(), OrbitError> {
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(layout_io_error("read task event history", path, error)),
    };
    let mut migrated = String::with_capacity(raw.len());
    let mut changed = false;

    for chunk in raw.split_inclusive('\n') {
        // An unterminated final row is a crash tail ignored by the bundle
        // reader. Preserve it byte-for-byte so this migration does not turn a
        // partial append into an apparently committed event.
        if !chunk.ends_with('\n') {
            migrated.push_str(chunk);
            continue;
        }
        let line = chunk
            .strip_suffix('\n')
            .unwrap_or(chunk)
            .strip_suffix('\r')
            .unwrap_or_else(|| chunk.strip_suffix('\n').unwrap_or(chunk));
        let mut value: serde_json::Value = serde_json::from_str(line).map_err(|error| {
            OrbitError::Migration(format!(
                "cannot parse task event history row at '{}': {error}",
                path.display()
            ))
        })?;
        let row_changed = value.as_object_mut().is_some_and(|object| {
            replace_json_string_field(object, "from_status", "friction", "archived")
                | replace_json_string_field(object, "to_status", "friction", "archived")
        });
        if row_changed {
            migrated.push_str(&serde_json::to_string(&value).map_err(|error| {
                OrbitError::Migration(format!(
                    "cannot serialize migrated task event history '{}': {error}",
                    path.display()
                ))
            })?);
            if chunk.ends_with("\r\n") {
                migrated.push('\r');
            }
            migrated.push('\n');
            changed = true;
        } else {
            migrated.push_str(chunk);
        }
    }

    if !changed {
        return Ok(());
    }
    atomic_write_text(path, &migrated)
        .map_err(|error| layout_io_error("rewrite task event history", path, error))
}

fn replace_string_field(
    mapping: &mut serde_yaml::Mapping,
    field: &str,
    old: &str,
    new: &str,
) -> bool {
    let key = serde_yaml::Value::String(field.to_string());
    let Some(value) = mapping.get_mut(&key) else {
        return false;
    };
    if value.as_str() != Some(old) {
        return false;
    }
    *value = serde_yaml::Value::String(new.to_string());
    true
}

fn replace_json_string_field(
    object: &mut serde_json::Map<String, serde_json::Value>,
    field: &str,
    old: &str,
    new: &str,
) -> bool {
    let Some(value) = object.get_mut(field) else {
        return false;
    };
    if value.as_str() != Some(old) {
        return false;
    }
    *value = serde_json::Value::String(new.to_string());
    true
}

fn layout_io_error(operation: &str, path: &Path, error: std::io::Error) -> OrbitError {
    OrbitError::Migration(format!("{operation} '{}': {error}", path.display()))
}

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
    /// only by additive migrations (ORB-12434). The workspace is usable
    /// read-only: nothing was applied and the marker was not rewritten.
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
/// `Ok(Some(report))` when it is newer only by additive migrations (nothing
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
/// may open the newer workspace read-only. The outer error covers only I/O
/// on the record itself; an unusable record is an inner [`CompatibilityRefusal`].
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

fn marker_path(orbit_dir: &Path) -> PathBuf {
    orbit_dir.join("state").join(MARKER_FILE)
}

fn upgrade_lock_path(orbit_dir: &Path) -> PathBuf {
    orbit_dir.join("state").join(UPGRADE_LOCK_FILE)
}

fn compat_path(orbit_dir: &Path) -> PathBuf {
    orbit_dir.join("state").join(COMPAT_FILE)
}

/// Read the companion compatibility record. A missing file is the
/// pre-ORB-12434 case, not an error.
fn read_compat_record(
    orbit_dir: &Path,
) -> Result<Result<Option<CompatibilityRecord>, CompatibilityRefusal>, OrbitError> {
    let path = compat_path(orbit_dir);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Ok(None)),
        Err(error) => {
            return Err(OrbitError::Migration(format!(
                "cannot read layout compatibility record '{}': {error}",
                path.display()
            )));
        }
    };
    Ok(CompatibilityRecord::decode(raw.trim()).map(Some))
}

/// Record what this binary knows about layout compatibility, so a binary
/// that is older than `version` can tell whether it may still read the
/// workspace. Written after the marker: a crash in between leaves a stale
/// record, which readers refuse rather than misinterpret.
fn write_compat_record(
    orbit_dir: &Path,
    migrations: &[LayoutMigration],
    version: u32,
) -> Result<(), OrbitError> {
    let record = CompatibilityRecord::for_registry(
        version,
        migrations
            .iter()
            .map(|migration| (migration.version, migration.name, migration.compat)),
    );
    let path = compat_path(orbit_dir);
    atomic_write_text(&path, &format!("{}\n", record.encode()?)).map_err(|error| {
        OrbitError::Migration(format!(
            "cannot write layout compatibility record '{}': {error}",
            path.display()
        ))
    })
}

fn read_marker(orbit_dir: &Path) -> Result<u32, OrbitError> {
    let path = marker_path(orbit_dir);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => {
            return Err(OrbitError::Migration(format!(
                "cannot read layout version marker '{}': {error}",
                path.display()
            )));
        }
    };
    raw.trim().parse::<u32>().map_err(|_| {
        OrbitError::Migration(format!(
            "corrupt layout version marker '{}': {:?} is not a version number \
             (delete the file to re-adopt from version 0 — migrations are idempotent)",
            path.display(),
            raw.trim()
        ))
    })
}

/// Advance the marker atomically (temp file + rename), so a crash mid-write
/// leaves either the old or the new version, never a torn marker.
fn write_marker(orbit_dir: &Path, version: u32) -> Result<(), OrbitError> {
    let path = marker_path(orbit_dir);
    let map_err = |op: &str, error: std::io::Error| {
        OrbitError::Migration(format!(
            "cannot {op} layout version marker '{}': {error}",
            path.display()
        ))
    };
    if let Some(parent) = path.parent() {
        create_private_dir_all(parent).map_err(|e| map_err("create directory for", e))?;
    }
    let tmp = path.with_extension("version.tmp");
    std::fs::write(&tmp, format!("{version}\n")).map_err(|e| map_err("stage", e))?;
    std::fs::rename(&tmp, &path).map_err(|e| map_err("commit", e))
}

#[cfg(test)]
mod tests;
