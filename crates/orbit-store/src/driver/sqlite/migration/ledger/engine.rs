use std::path::Path;

use orbit_common::storage::sqlite::{sqlite_error, sqlite_store_error};
use orbit_common::{OrbitError, SqliteContention, StorageLayer};
use rusqlite::{Connection, ErrorCode, Transaction, TransactionBehavior, params};

use super::compatibility::write_compat_record;
use super::{
    AppliedMigration, LedgeredDatabase, Migration, STORE_DATABASE, evaluate_newer_database,
};
use crate::contracts::ForwardCompatibleOpen;

const LEDGER_KEY_PREFIX: &str = "migration.v";

/// Bring `conn` up to the newest version in `migrations`, recording each
/// applied migration in the ledger.
///
/// A database newer than the registry supports is decided by its
/// forward-compatibility record: `Ok(Some(..))` describes that newer open —
/// the caller must keep the connection read-only unless it is
/// [`ForwardCompatibleOpen::writable`] (ORB-12434); an error means this binary
/// must not touch the database at all.
pub(crate) fn run_migrations(
    conn: &Connection,
    migrations: &[Migration],
) -> Result<Option<ForwardCompatibleOpen>, OrbitError> {
    run_database_migrations(conn, &STORE_DATABASE, migrations, None)
}

pub(crate) fn run_migrations_at_path(
    conn: &Connection,
    migrations: &[Migration],
    path: &Path,
) -> Result<Option<ForwardCompatibleOpen>, OrbitError> {
    run_database_migrations(conn, &STORE_DATABASE, migrations, Some(path))
}

/// Apply a database's ordered registry through its own transactional ledger.
/// `path`, when known, names the file in a contention error.
///
/// Returns `None` for a current or migrated database. For a newer database,
/// returns its compatibility decision without migrating: the caller must hold
/// the connection read-only unless [`ForwardCompatibleOpen::writable`] is true.
/// An incompatible or unclassified newer schema is refused before any write.
pub fn run_database_migrations(
    conn: &Connection,
    database: &LedgeredDatabase,
    migrations: &[Migration],
    path: Option<&Path>,
) -> Result<Option<ForwardCompatibleOpen>, OrbitError> {
    validate_registry(migrations)?;
    let current = current_schema_version(conn)?;
    let supported = migrations.last().map(|m| m.version).unwrap_or(0);
    if current > supported {
        return evaluate_newer_database(conn, database, current, supported).map(Some);
    }
    // A current database needs no write transaction. Return before the
    // idempotent CREATE TABLE so read-only mounts remain genuinely readable.
    if current == supported {
        return Ok(None);
    }

    // `current` is only a hint for which versions to attempt. `apply_one`
    // re-reads the ledger under BEGIN IMMEDIATE and skips anything another
    // opener already committed while this connection waited.
    for migration in migrations.iter().filter(|m| m.version > current) {
        if let Some(forward) = apply_one(conn, database, migrations, migration, supported, path)? {
            // Another opener advanced the database past this binary while we
            // were applying; the rest of the registry is moot and the caller
            // must hold the connection read-only.
            return Ok(Some(forward));
        }
    }

    Ok(None)
}

/// Current schema version recorded in the ledger; 0 when no versioned
/// migration has been applied (fresh or pre-ledger legacy database).
pub fn current_schema_version(conn: &Connection) -> Result<u32, OrbitError> {
    Ok(applied_migrations(conn)?
        .last()
        .map(|m| m.version)
        .unwrap_or(0))
}

/// All migrations recorded as applied, ordered by version ascending.
pub(crate) fn applied_migrations(conn: &Connection) -> Result<Vec<AppliedMigration>, OrbitError> {
    if !super::super::table_exists(conn, "schema_meta")? {
        return Ok(Vec::new());
    }

    let mut stmt = conn
        .prepare(
            "SELECT key, value, updated_at FROM schema_meta
             WHERE key LIKE ?1 ORDER BY key",
        )
        .map_err(sqlite_store_error)?;
    let rows = stmt
        .query_map([format!("{LEDGER_KEY_PREFIX}%")], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(sqlite_store_error)?;

    let mut applied = Vec::new();
    for row in rows {
        let (key, name, applied_at) = row.map_err(sqlite_store_error)?;
        let version = parse_ledger_key(&key)?;
        applied.push(AppliedMigration {
            version,
            name,
            applied_at,
        });
    }
    applied.sort_by_key(|m| m.version);
    Ok(applied)
}

/// Apply one migration, unless the database has meanwhile moved past this
/// binary — in which case the forward-compatibility decision is returned
/// instead (or raised as a refusal).
fn apply_one(
    conn: &Connection,
    database: &LedgeredDatabase,
    migrations: &[Migration],
    migration: &Migration,
    supported: u32,
    path: Option<&Path>,
) -> Result<Option<ForwardCompatibleOpen>, OrbitError> {
    // Declared before the transaction so it is restored after the
    // transaction commits or rolls back.
    let _foreign_keys = database
        .foreign_keys_off
        .then(|| ForeignKeysOff::new(conn))
        .transpose()?;
    // BEGIN IMMEDIATE takes the reserved lock before any read so a WAL
    // snapshot cannot be pinned under DEFERRED while another opener
    // commits. `new_unchecked` is the `&Connection` form of
    // `transaction_with_behavior(TransactionBehavior::Immediate)`.
    // Drop rolls back, so a failure or panic leaves neither partial
    // schema nor a ledger row behind.
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .map_err(|error| migration_begin_error(path, migration, error))?;

    let current = current_schema_version(&tx)?;
    if current > supported {
        // Another opener advanced the database past this binary while we
        // waited for the write lock. Re-decide from its record rather than
        // continuing to apply migrations to a newer database.
        return evaluate_newer_database(&tx, database, current, supported).map(Some);
    }
    if current >= migration.version {
        return Ok(None);
    }

    ensure_schema_meta_table(&tx)?;

    (migration.apply)(&tx).map_err(|error| {
        OrbitError::storage(
            StorageLayer::Migration,
            error.is_readonly_or_access_failure(),
            format!(
                "failed to apply migration v{} ({}): {error}",
                migration.version, migration.name
            ),
        )
    })?;

    tx.execute(
        "INSERT INTO schema_meta(key, value, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        params![
            ledger_key(migration.version),
            migration.name,
            crate::now_string()
        ],
    )
    .map_err(|e| {
        sqlite_error(
            StorageLayer::Migration,
            &e,
            format!(
                "failed to record migration v{} ({}) in schema_meta: {e}",
                migration.version, migration.name
            ),
        )
    })?;
    write_compat_record(&tx, migrations, migration.version)?;
    if let Some(stamp) = database.stamp {
        stamp(&tx, migrations, migration.version)?;
    }

    tx.commit()
        .map_err(|error| commit_migration_error(migration, error))?;

    orbit_common::tracing::info!(
        target: "orbit.store.sqlite",
        database = database.noun,
        version = migration.version,
        name = migration.name,
        "applied schema migration",
    );
    Ok(None)
}

fn migration_begin_error(
    path: Option<&Path>,
    migration: &Migration,
    error: rusqlite::Error,
) -> OrbitError {
    if matches!(
        error.sqlite_error_code(),
        Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
    ) && let Some(path) = path
    {
        return OrbitError::SqliteContention(Box::new(SqliteContention {
            path: path.display().to_string(),
            phase: format!(
                "begin migration v{} ({})",
                migration.version, migration.name
            ),
            detail: error.to_string(),
        }));
    }

    sqlite_error(
        StorageLayer::Migration,
        &error,
        format!(
            "failed to begin transaction for migration v{} ({}): {error}",
            migration.version, migration.name
        ),
    )
}

/// Foreign-key enforcement turned off on one connection until dropped.
struct ForeignKeysOff<'a> {
    conn: &'a Connection,
}

impl<'a> ForeignKeysOff<'a> {
    fn new(conn: &'a Connection) -> Result<Self, OrbitError> {
        conn.pragma_update(None, "foreign_keys", "OFF")
            .map_err(|error| {
                sqlite_error(
                    StorageLayer::Migration,
                    &error,
                    format!("failed to turn off foreign keys for a migration: {error}"),
                )
            })?;
        Ok(Self { conn })
    }
}

impl Drop for ForeignKeysOff<'_> {
    fn drop(&mut self) {
        if let Err(error) = self.conn.pragma_update(None, "foreign_keys", "ON") {
            orbit_common::tracing::warn!(
                target: "orbit.store.sqlite",
                error = %error,
                "could not turn foreign keys back on after a migration",
            );
        }
    }
}

pub(super) fn commit_migration_error(migration: &Migration, error: rusqlite::Error) -> OrbitError {
    if matches!(
        &error,
        rusqlite::Error::SqliteFailure(sqlite_error, _)
            if sqlite_error.code == ErrorCode::DiskFull
    ) {
        return sqlite_error(
            StorageLayer::Store,
            &error,
            format!(
                "SQLITE_FULL / ENOSPC while committing migration v{} ({}): {error}",
                migration.version, migration.name
            ),
        );
    }

    sqlite_error(
        StorageLayer::Migration,
        &error,
        format!(
            "failed to commit migration v{} ({}): {error}",
            migration.version, migration.name
        ),
    )
}

fn ensure_schema_meta_table(conn: &Connection) -> Result<(), OrbitError> {
    // Bootstrap table for the ledger itself; must match the shape the
    // baseline schema declares (also used for state-import markers).
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_meta (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );",
    )
    .map_err(sqlite_store_error)
}

fn validate_registry(migrations: &[Migration]) -> Result<(), OrbitError> {
    let mut previous = 0u32;
    for migration in migrations {
        if migration.version <= previous {
            return Err(OrbitError::Migration(format!(
                "migration registry is not strictly increasing: v{} ({}) follows v{previous}",
                migration.version, migration.name
            )));
        }
        previous = migration.version;
    }
    Ok(())
}

fn ledger_key(version: u32) -> String {
    format!("{LEDGER_KEY_PREFIX}{version:04}")
}

fn parse_ledger_key(key: &str) -> Result<u32, OrbitError> {
    key.strip_prefix(LEDGER_KEY_PREFIX)
        .and_then(|suffix| suffix.parse::<u32>().ok())
        .ok_or_else(|| {
            OrbitError::Migration(format!(
                "corrupt schema_meta migration ledger entry: {key:?}"
            ))
        })
}
