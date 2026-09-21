//! Versioned schema-migration ledger for the SQLite store (ORB-10003).
//!
//! Migrations are a stable, ordered registry of `(version, name, apply)`
//! entries. Each applied migration is recorded in the `schema_meta`
//! key/value table under `migration.v<NNNN>` (value = migration name,
//! `updated_at` = applied-at timestamp), so the ledger doubles as the
//! data source for a future `orbit migrate` command (P3.4).
//!
//! Guarantees:
//! - Each migration runs inside a single transaction together with its
//!   ledger insert, so an interrupted migration rolls back instead of
//!   leaving half-applied schema (SQLite ALTER/rename-copy-drop batches
//!   are wrappable in a transaction).
//! - Concurrent openers serialize on `BEGIN IMMEDIATE` and re-read the
//!   ledger inside that transaction, so a waiter neither hits
//!   `SQLITE_BUSY_SNAPSHOT` nor re-applies a version another process
//!   just committed.
//! - A database whose recorded version is newer than
//!   [`SUPPORTED_SCHEMA_VERSION`] is decided from the
//!   `schema_meta` forward-compatibility record a newer binary leaves
//!   behind (ORB-12434): newer by additive migrations only opens
//!   read-only, anything else is refused with [`OrbitError::Migration`]
//!   naming the first breaking migration this binary lacks.
//! - Legacy databases created by the pre-ledger idempotent migrations
//!   adopt the ledger transparently: the v1 baseline is the same
//!   idempotent schema code, so running it on an existing database is a
//!   no-op that then records version 1.

use std::path::Path;

use orbit_common::{OrbitError, SqliteContention};
use rusqlite::{Connection, ErrorCode, Transaction, TransactionBehavior, params};

use crate::contracts::{
    CompatibilityRecord, CompatibilityRefusal, ForwardCompatibleOpen, MigrationCompatibility,
    StateComponent, evaluate_newer_state,
};

/// One entry in the migration registry.
pub(crate) struct Migration {
    pub(crate) version: u32,
    pub(crate) name: &'static str,
    /// What this migration means for a binary that does not have it. See
    /// [`MigrationCompatibility`]; declare `Breaking` when in doubt.
    pub(crate) compat: MigrationCompatibility,
    pub(crate) apply: fn(&Connection) -> Result<(), OrbitError>,
}

/// Stable ordered registry of store-database migrations. Append-only:
/// never renumber or edit an entry that has shipped.
// L-0083: Preserve every shipped migration entry across reverts and history rewrites.
pub(crate) const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "baseline",
        compat: MigrationCompatibility::Additive,
        apply: super::apply_baseline_schema,
    },
    Migration {
        version: 2,
        name: "learnings_index_workspace_scope",
        // Drops and reshapes `learnings_index`, which binaries without it query.
        compat: MigrationCompatibility::Breaking,
        apply: super::apply_learning_index_workspace_scope,
    },
    Migration {
        version: 3,
        name: "flat_crew_model",
        compat: MigrationCompatibility::Additive,
        apply: super::apply_flat_crew_model,
    },
    Migration {
        version: 4,
        name: "job_run_archive_stage",
        compat: MigrationCompatibility::Additive,
        apply: super::apply_job_run_archive_stage,
    },
    Migration {
        version: 5,
        name: "host_registry_core",
        compat: MigrationCompatibility::Additive,
        apply: super::apply_host_registry_core,
    },
    Migration {
        version: 6,
        name: "workspace_coordination_projections",
        compat: MigrationCompatibility::Additive,
        apply: super::apply_workspace_coordination_projections,
    },
    Migration {
        version: 7,
        name: "trusted_mcp_audit_provenance",
        compat: MigrationCompatibility::Additive,
        apply: super::apply_trusted_mcp_audit_provenance,
    },
    Migration {
        version: 8,
        name: "hub_registry_metadata",
        compat: MigrationCompatibility::Additive,
        apply: super::apply_hub_registry_metadata,
    },
    Migration {
        version: 9,
        name: "feature_schema_ledger",
        compat: MigrationCompatibility::Additive,
        apply: super::apply_feature_schema_ledger,
    },
    // ORB-10367: carries the invocation telemetry columns
    // (`cache_create_1h_tokens`, `provider_cost_usd`) to databases that
    // recorded the v1 baseline before those columns were added to it.
    Migration {
        version: 10,
        name: "invocation_telemetry_columns",
        compat: MigrationCompatibility::Additive,
        apply: super::apply_invocation_telemetry_columns,
    },
    // ADR-0287: baseline v1 is frozen; schema added later always advances
    // through an append-only ledger entry.
    Migration {
        version: 11,
        name: "routine_scheduler_schema",
        compat: MigrationCompatibility::Additive,
        apply: super::apply_routine_scheduler_schema,
    },
    // ORB-10680: hub friction records leave the Markdown tree for the
    // host-global store, keyed by `(workspace_id, friction_id)`.
    Migration {
        version: 12,
        name: "friction_records_sqlite",
        compat: MigrationCompatibility::Additive,
        apply: super::apply_friction_records_schema,
    },
    // ORB-10709 / ADR-0352: `task_reservations` gains the coordination
    // dimension that separates worker file reservations from the exclusive
    // workspace claim, plus the claim's bearer token.
    Migration {
        version: 13,
        name: "workspace_claim_scope",
        // Adds the coordination dimension to `task_reservations`: a binary
        // without it reads an exclusive workspace claim as a file reservation.
        compat: MigrationCompatibility::Breaking,
        apply: super::apply_workspace_claim_scope,
    },
    // ORB-10736: keep the shipped learning migrations above intact, then
    // retire their projections explicitly for both upgraded and fresh stores.
    Migration {
        version: 14,
        name: "remove_native_learning_subsystem",
        // Drops the learning tables older binaries still query.
        compat: MigrationCompatibility::Breaking,
        apply: super::apply_remove_native_learning_subsystem,
    },
    Migration {
        version: 15,
        name: "invocation_audit_context",
        compat: MigrationCompatibility::Additive,
        apply: super::apply_invocation_audit_context,
    },
    // ORB-10888: `role` alone conflates agent families, model strings, system
    // markers, and unattributed markers. The canonical actor projection lands
    // beside it and is backfilled for existing rows.
    Migration {
        version: 16,
        name: "audit_actor_identity",
        compat: MigrationCompatibility::Additive,
        apply: super::apply_audit_actor_identity,
    },
    // ORB-10890: the untrusted half of attribution. An MCP client started from
    // its own config cannot satisfy the managed-run trust boundary, so its
    // self-declared identity lands beside `role` rather than in it.
    Migration {
        version: 17,
        name: "audit_self_reported_actor",
        compat: MigrationCompatibility::Additive,
        apply: super::apply_audit_self_reported_actor,
    },
    // Alias map v2: `fable` became a family rule so versioned Fable labels
    // resolve to `claude`. Rows stamped with the old map are re-derived.
    Migration {
        version: 18,
        name: "audit_actor_alias_v2",
        compat: MigrationCompatibility::Additive,
        apply: super::apply_audit_actor_alias_v2,
    },
    // The run listing orders by `created_at DESC, run_id` per workspace; the
    // existing indexes cover `(workspace_id, job_id, scheduled_at)` and
    // `(workspace_id, state)`, so every dashboard page scanned and sorted a
    // workspace's whole run history.
    Migration {
        version: 19,
        name: "job_runs_created_index",
        compat: MigrationCompatibility::Additive,
        apply: super::apply_job_runs_created_index,
    },
    // Every window filter (`ts >= ? AND ts < ?`) and the newest-first
    // listing (`ORDER BY ts DESC, id DESC LIMIT n`) over `invocations` had
    // only the job-run and activity indexes to work with, so each was a
    // full scan plus a temp sort.
    Migration {
        version: 20,
        name: "invocations_ts_index",
        compat: MigrationCompatibility::Additive,
        apply: super::apply_invocations_ts_index,
    },
    // ORB-12528: the durable commit decision that lets one task transition,
    // its history, a reservation, and dependent coordination rows be
    // published as a single outcome across the bundle files and this
    // database. Additive: an older binary ignores both tables.
    Migration {
        version: 21,
        name: "task_commit_journal",
        compat: MigrationCompatibility::Additive,
        apply: super::apply_task_commit_journal,
    },
    Migration {
        version: 22,
        name: "execution_provenance",
        compat: MigrationCompatibility::Additive,
        apply: super::apply_execution_provenance,
    },
    // ORB-12725: `caller_host_id`/`process_host_id` carry a machine's display
    // name, so they are renamed to `caller_machine_name`/`process_machine_name`
    // with the rest of the host -> machine vocabulary. Breaking: an older
    // binary selects the retired column names by name and would fail at the
    // first audit read rather than silently losing attribution.
    Migration {
        version: 23,
        name: "audit_machine_name_columns",
        compat: MigrationCompatibility::Breaking,
        apply: super::apply_audit_machine_name_columns,
    },
    // Plugin standard phase 1: the `plugins` table beside `tools`, plus the
    // audit columns carrying plugin name, version and manifest digest.
    Migration {
        version: 24,
        name: "plugins_and_audit_plugin_provenance",
        compat: MigrationCompatibility::Additive,
        apply: super::apply_plugins_and_audit_plugin_provenance,
    },
    // Plugin standard phase 2: the grant set a plugin call ran under.
    Migration {
        version: 25,
        name: "audit_plugin_grants",
        compat: MigrationCompatibility::Additive,
        apply: super::apply_audit_plugin_grants,
    },
    // Operation mode was removed on 2026-09-21 (ORB-12772): the grant and
    // recovery-ledger tables its `operation` feature migration created go
    // with it. Breaking: an older binary reads `operation_grants` at handoff
    // commit and would fail there rather than silently skipping the check.
    Migration {
        version: 26,
        name: "remove_operation_mode",
        compat: MigrationCompatibility::Breaking,
        apply: super::apply_remove_operation_mode,
    },
];

/// Highest schema version this binary knows how to produce. Public for
/// the future `orbit migrate` surface (P3.4), alongside
/// [`AppliedMigration`] and the `Store` version accessors.
pub const SUPPORTED_SCHEMA_VERSION: u32 = 26;

const LEDGER_KEY_PREFIX: &str = "migration.v";

/// `schema_meta` key carrying the forward-compatibility record (ORB-12434).
/// Deliberately outside the `migration.v<NNNN>` namespace the ledger scans.
pub(crate) const COMPAT_KEY: &str = "migration.compat";

/// A migration recorded as applied in the `schema_meta` ledger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedMigration {
    pub version: u32,
    pub name: String,
    pub applied_at: String,
}

/// Bring `conn` up to the newest version in `migrations`, recording each
/// applied migration in the ledger.
///
/// A database newer than the registry supports is decided by its
/// forward-compatibility record: `Ok(Some(..))` means the caller must keep
/// the connection read-only (ORB-12434); an error means this binary must not
/// touch the database at all.
pub(crate) fn run_migrations(
    conn: &Connection,
    migrations: &[Migration],
) -> Result<Option<ForwardCompatibleOpen>, OrbitError> {
    run_migrations_inner(conn, migrations, None)
}

pub(crate) fn run_migrations_at_path(
    conn: &Connection,
    migrations: &[Migration],
    path: &Path,
) -> Result<Option<ForwardCompatibleOpen>, OrbitError> {
    run_migrations_inner(conn, migrations, Some(path))
}

fn run_migrations_inner(
    conn: &Connection,
    migrations: &[Migration],
    path: Option<&Path>,
) -> Result<Option<ForwardCompatibleOpen>, OrbitError> {
    validate_registry(migrations)?;
    let current = current_schema_version(conn)?;
    let supported = migrations.last().map(|m| m.version).unwrap_or(0);
    if current > supported {
        return evaluate_newer_database(conn, current, supported).map(Some);
    }
    // A current store needs no write transaction. Return before the
    // idempotent CREATE TABLE so read-only mounts remain genuinely readable.
    if current == supported {
        return Ok(None);
    }

    // `current` is only a hint for which versions to attempt. `apply_one`
    // re-reads the ledger under BEGIN IMMEDIATE and skips anything another
    // opener already committed while this connection waited.
    for migration in migrations.iter().filter(|m| m.version > current) {
        if let Some(forward) = apply_one(conn, migrations, migration, supported, path)? {
            // Another opener advanced the database past this binary while we
            // were applying; the rest of the registry is moot and the caller
            // must hold the connection read-only.
            return Ok(Some(forward));
        }
    }

    Ok(None)
}

/// Decide a database recorded newer than this binary supports. Reads only
/// the compatibility record a newer binary left behind; never writes.
fn evaluate_newer_database(
    conn: &Connection,
    current: u32,
    supported: u32,
) -> Result<ForwardCompatibleOpen, OrbitError> {
    let record = match read_compat_record(conn) {
        Ok(record) => evaluate_newer_state(StateComponent::StoreSchema, current, supported, record),
        Err(refusal) => Err(refusal),
    };
    match record {
        Ok(forward) => {
            orbit_common::tracing::warn!(
                target: "orbit.store.sqlite",
                schema_version = current,
                supported_version = supported,
                "opening a newer store database read-only; this binary applies no schema migration to it",
            );
            Ok(forward)
        }
        Err(refusal) => Err(newer_than_supported(current, supported, &refusal)),
    }
}

/// Read the forward-compatibility record from `schema_meta`. A missing row
/// is the pre-ORB-12434 case, not an error.
pub(crate) fn read_compat_record(
    conn: &Connection,
) -> Result<Option<CompatibilityRecord>, CompatibilityRefusal> {
    let raw = match conn.query_row(
        "SELECT value FROM schema_meta WHERE key = ?1",
        [COMPAT_KEY],
        |row| row.get::<_, String>(0),
    ) {
        Ok(raw) => raw,
        Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
        Err(error) => return Err(CompatibilityRefusal::CorruptRecord(error.to_string())),
    };
    CompatibilityRecord::decode(raw.trim()).map(Some)
}

/// Record what this binary knows about schema compatibility, inside the same
/// transaction that commits the migration it describes.
fn write_compat_record(
    conn: &Connection,
    migrations: &[Migration],
    version: u32,
) -> Result<(), OrbitError> {
    let record = CompatibilityRecord::for_registry(
        version,
        migrations
            .iter()
            .map(|migration| (migration.version, migration.name, migration.compat)),
    );
    conn.execute(
        "INSERT INTO schema_meta(key, value, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        params![COMPAT_KEY, record.encode()?, crate::now_string()],
    )
    .map_err(|error| {
        OrbitError::Migration(format!(
            "failed to record schema compatibility metadata at v{version}: {error}"
        ))
    })?;
    Ok(())
}

/// Current schema version recorded in the ledger; 0 when no versioned
/// migration has been applied (fresh or pre-ledger legacy database).
pub(crate) fn current_schema_version(conn: &Connection) -> Result<u32, OrbitError> {
    Ok(applied_migrations(conn)?
        .last()
        .map(|m| m.version)
        .unwrap_or(0))
}

/// All migrations recorded as applied, ordered by version ascending.
pub(crate) fn applied_migrations(conn: &Connection) -> Result<Vec<AppliedMigration>, OrbitError> {
    if !super::table_exists(conn, "schema_meta")? {
        return Ok(Vec::new());
    }

    let mut stmt = conn
        .prepare(
            "SELECT key, value, updated_at FROM schema_meta
             WHERE key LIKE ?1 ORDER BY key",
        )
        .map_err(|e| OrbitError::Store(e.to_string()))?;
    let rows = stmt
        .query_map([format!("{LEDGER_KEY_PREFIX}%")], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(|e| OrbitError::Store(e.to_string()))?;

    let mut applied = Vec::new();
    for row in rows {
        let (key, name, applied_at) = row.map_err(|e| OrbitError::Store(e.to_string()))?;
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
    migrations: &[Migration],
    migration: &Migration,
    supported: u32,
    path: Option<&Path>,
) -> Result<Option<ForwardCompatibleOpen>, OrbitError> {
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
        return evaluate_newer_database(&tx, current, supported).map(Some);
    }
    if current >= migration.version {
        return Ok(None);
    }

    ensure_schema_meta_table(&tx)?;

    (migration.apply)(&tx).map_err(|error| {
        OrbitError::Migration(format!(
            "failed to apply migration v{} ({}): {error}",
            migration.version, migration.name
        ))
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
        OrbitError::Migration(format!(
            "failed to record migration v{} ({}) in schema_meta: {e}",
            migration.version, migration.name
        ))
    })?;
    write_compat_record(&tx, migrations, migration.version)?;

    tx.commit()
        .map_err(|error| commit_migration_error(migration, error))?;

    orbit_common::tracing::info!(
        target: "orbit.store.sqlite",
        version = migration.version,
        name = migration.name,
        "applied store schema migration",
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

    OrbitError::Migration(format!(
        "failed to begin transaction for migration v{} ({}): {error}",
        migration.version, migration.name
    ))
}

fn newer_than_supported(
    current: u32,
    supported: u32,
    refusal: &CompatibilityRefusal,
) -> OrbitError {
    OrbitError::Migration(format!(
        "store database schema version {current} is newer than the newest version this \
        orbit binary supports ({supported}); {refusal}; upgrade orbit to open this database"
    ))
}

pub(super) fn commit_migration_error(migration: &Migration, error: rusqlite::Error) -> OrbitError {
    if matches!(
        &error,
        rusqlite::Error::SqliteFailure(sqlite_error, _)
            if sqlite_error.code == ErrorCode::DiskFull
    ) {
        return OrbitError::Store(format!(
            "SQLITE_FULL / ENOSPC while committing migration v{} ({}): {error}",
            migration.version, migration.name
        ));
    }

    OrbitError::Migration(format!(
        "failed to commit migration v{} ({}): {error}",
        migration.version, migration.name
    ))
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
    .map_err(|e| OrbitError::Store(e.to_string()))
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
