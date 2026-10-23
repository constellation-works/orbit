use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::StorageLayer;
use orbit_common::storage::sqlite::{sqlite_error, sqlite_store_error};
use rusqlite::{Connection, Transaction, TransactionBehavior};

use super::allocator::HIGH_WATER_UNKNOWN_AUTHORITY;
use super::baseline::is_adoptable;
use super::ledger::{
    MIGRATIONS, PRE_LEDGER_COMPATIBLE, REGISTRY_DATABASE, SUPPORTED_REGISTRY_VERSION,
};
use super::util::now_string;
use crate::contracts::ForwardCompatibleOpen;
use crate::driver::sqlite::migration::{
    current_schema_version, evaluate_newer_database, run_database_migrations,
};

/// How a registry opened for observation may be read.
pub(super) struct ReadableRegistry {
    /// The registry has no ledger yet: it is a pre-ledger v5 registry, so
    /// tables its baseline would create may be missing.
    pub(super) pre_ledger: bool,
}

/// Bring a writable registry onto this binary's ledger, adopting a
/// pre-ledger registry by running its baseline, then seed the allocator.
///
/// `Ok(Some(..))` is a newer registry this binary may open: the caller must
/// hold it read-only unless it is [`ForwardCompatibleOpen::writable`].
/// `workspaces_dir` is consulted only when the allocator still needs seeding.
pub(super) fn migrate_writable(
    conn: &Connection,
    path: &Path,
    workspaces_dir: &Path,
) -> Result<Option<ForwardCompatibleOpen>, OrbitError> {
    if current_schema_version(conn)? == 0 && !is_adoptable(conn)? {
        return Err(unsupported_schema(registry_user_version(conn)?, path));
    }
    let forward = run_database_migrations(conn, &REGISTRY_DATABASE, MIGRATIONS, Some(path))?;
    if forward.as_ref().is_none_or(|forward| forward.writable) && !allocator_seeded(conn)? {
        seed_allocator(conn, holds_partitions(workspaces_dir)?)?;
    }
    Ok(forward)
}

/// Confirm a registry opened for observation is readable as-is.
///
/// Adoption and migration need a write transaction, so on read-only storage
/// there is nothing to attempt: naming the writable step the registry still
/// owes beats an opaque SQLite write error raised from a transaction that
/// could only fail. A newer registry is decided from its compatibility
/// record; any open it allows is a read.
pub(super) fn assert_readable_schema(
    conn: &Connection,
    path: &Path,
) -> Result<ReadableRegistry, OrbitError> {
    let version = current_schema_version(conn)?;
    if version > SUPPORTED_REGISTRY_VERSION {
        evaluate_newer_database(
            conn,
            &REGISTRY_DATABASE,
            version,
            SUPPORTED_REGISTRY_VERSION,
        )?;
        return Ok(ReadableRegistry { pre_ledger: false });
    }
    if version == SUPPORTED_REGISTRY_VERSION {
        return Ok(ReadableRegistry { pre_ledger: false });
    }
    if version > 0 {
        return Err(OrbitError::Store(format!(
            "task registry '{}' requires a writable migration: the database is read-only, and \
             observing it found registry ledger version {version} instead of version \
             {SUPPORTED_REGISTRY_VERSION}. Open the registry once from writable storage, then \
             retry the observation",
            path.display()
        )));
    }

    // No ledger: a pre-ledger v5 registry with action keys reads as it did.
    let user_version = registry_user_version(conn)?;
    let action_keys = table_has_column(conn, "task_action_keys", "action_key")?;
    if user_version == PRE_LEDGER_COMPATIBLE && action_keys {
        return Ok(ReadableRegistry { pre_ledger: true });
    }
    if user_version > 6 {
        return Err(unsupported_schema(user_version, path));
    }
    let action_keys = if action_keys {
        ""
    } else {
        " without task action keys"
    };
    Err(OrbitError::Store(format!(
        "task registry '{}' requires writable additive setup/recovery: the database is read-only, \
         and observing it found schema version {user_version}{action_keys} instead of version \
         {PRE_LEDGER_COMPATIBLE} with task action keys. Open the registry once from writable \
         storage, then retry the observation",
        path.display()
    )))
}

/// Whether the bundle root already holds a task partition. Partitions are
/// created only through an open registry, so one that predates the registry's
/// schema means the registry holding its allocator was lost or replaced.
fn holds_partitions(workspaces_dir: &Path) -> Result<bool, OrbitError> {
    let entries = match std::fs::read_dir(workspaces_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(OrbitError::Io(error.to_string())),
    };
    for entry in entries {
        let entry = entry.map_err(|error| OrbitError::Io(error.to_string()))?;
        if entry
            .file_type()
            .map_err(|error| OrbitError::Io(error.to_string()))?
            .is_dir()
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn allocator_seeded(conn: &Connection) -> Result<bool, OrbitError> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM allocator_state WHERE authority = 'local')",
        [],
        |row| row.get(0),
    )
    .map_err(sqlite_store_error)
}

/// Seed the local allocator row, and mark it in the same transaction when it
/// was created over existing partitions, so no concurrent opener can allocate
/// from the unmarked zero counter. Every writable open that finds the row
/// missing seeds it, so an opener never allocates before some opener seeded.
///
/// `prior_partitions` says the bundle root already held task partitions when
/// this registry was opened. A fresh allocator seeded over them cannot know
/// which ids were issued, so it is marked to refuse allocation until an
/// operator establishes the high-water mark.
fn seed_allocator(conn: &Connection, prior_partitions: bool) -> Result<(), OrbitError> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .map_err(sqlite_store_error)?;
    let now = now_string();
    let seeded = tx
        .execute(
            "INSERT OR IGNORE INTO allocator_state(authority, next_number, task_prefix, updated_at)
             VALUES ('local', 0, 'ORB', ?1)",
            [&now],
        )
        .map_err(sqlite_store_error)?;
    if seeded == 1 && prior_partitions {
        tx.execute(
            "INSERT OR IGNORE INTO allocator_state(authority, next_number, task_prefix, updated_at)
             VALUES (?1, 0, 'ORB', ?2)",
            [HIGH_WATER_UNKNOWN_AUTHORITY, &now],
        )
        .map_err(sqlite_store_error)?;
    }
    tx.commit().map_err(sqlite_store_error)
}

fn unsupported_schema(version: u32, path: &Path) -> OrbitError {
    let executable = std::env::current_exe()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| "unknown executable".into());
    OrbitError::Store(format!(
        "task registry schema version {version} is newer than supported version {PRE_LEDGER_COMPATIBLE} \
         or is not the known compatible additive format; registry '{}', executable '{}'. \
         Upgrade the selected Orbit executable and restart long-lived processes. \
         Check `command -v orbit` and `orbit --version` for mixed installations; \
         do not edit the database version manually",
        path.display(),
        executable
    ))
}

fn table_has_column(conn: &Connection, table: &str, column: &str) -> Result<bool, OrbitError> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(sqlite_store_error)?;
    let columns = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(sqlite_store_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sqlite_store_error)?;
    Ok(columns.iter().any(|candidate| candidate == column))
}

fn registry_user_version(conn: &Connection) -> Result<u32, OrbitError> {
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|e| {
            sqlite_error(
                StorageLayer::Store,
                &e,
                format!("failed to read registry user_version: {e}"),
            )
        })?;
    u32::try_from(version)
        .map_err(|e| OrbitError::Store(format!("invalid registry user_version {version}: {e}")))
}
