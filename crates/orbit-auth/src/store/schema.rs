//! Ordered schema migrations for the listener's lease token store.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::storage::compatibility::{MigrationCompatibility, StateComponent};
use orbit_common::storage::sqlite::{ledger, sqlite_store_error};
use rusqlite::Connection;

const DATABASE: ledger::LedgeredDatabase = ledger::LedgeredDatabase {
    component: StateComponent::FeatureSchema,
    noun: "lease token store",
    foreign_keys_off: false,
    stamp: None,
};

/// Append-only registry. Each migration owns its schema-defining code.
const MIGRATIONS: &[ledger::Migration] = &[ledger::Migration {
    version: 1,
    name: "lease_token_store_baseline",
    compat: MigrationCompatibility::Additive,
    apply: apply_lease_token_baseline,
}];

const SUPPORTED_VERSION: u32 = MIGRATIONS[MIGRATIONS.len() - 1].version;
const LAST_LEGACY_VERSION: i64 = 3;

// v1: tokens (hash only, never the secret) and their audit trail.
const LEGACY_TOKENS_DDL: &str = "CREATE TABLE lease_tokens (
         token_id        TEXT PRIMARY KEY,
         token_sha256    BLOB NOT NULL UNIQUE CHECK (length(token_sha256) = 32),
         principal       TEXT NOT NULL,
         principal_class TEXT NOT NULL CHECK (principal_class IN ('human', 'agent')),
         workspaces      TEXT NOT NULL,
         verbs           TEXT NOT NULL,
         max_priority    TEXT NOT NULL CHECK (max_priority IN ('low', 'normal', 'high')),
         hard            TEXT NOT NULL CHECK (hard IN ('allow', 'never')),
         created_at      TEXT NOT NULL,
         expires_at      TEXT NOT NULL,
         last_used_at    TEXT,
         revoked_at      TEXT
     );
     CREATE TABLE lease_token_audit (
         audit_id   INTEGER PRIMARY KEY AUTOINCREMENT,
         at         TEXT NOT NULL,
         event      TEXT NOT NULL CHECK (event IN ('mint', 'revoke', 'verify_refused')),
         token_id   TEXT,
         principal  TEXT,
         workspace  TEXT,
         verb       TEXT,
         reason     TEXT,
         actor      TEXT
     );";

// v2: listener refusals, with the remote address and route of the request.
const LEGACY_LISTENER_AUDIT_DDL: &str = "CREATE TABLE lease_token_audit_v2 (
         audit_id       INTEGER PRIMARY KEY AUTOINCREMENT,
         at             TEXT NOT NULL,
         event          TEXT NOT NULL
                        CHECK (event IN ('mint', 'revoke', 'verify_refused', 'listener_refused')),
         token_id       TEXT,
         principal      TEXT,
         workspace      TEXT,
         verb           TEXT,
         reason         TEXT,
         actor          TEXT,
         remote_address TEXT,
         route          TEXT
     );
     INSERT INTO lease_token_audit_v2
         (audit_id, at, event, token_id, principal, workspace, verb, reason, actor)
         SELECT audit_id, at, event, token_id, principal, workspace, verb, reason, actor
         FROM lease_token_audit;
     DROP TABLE lease_token_audit;
     ALTER TABLE lease_token_audit_v2 RENAME TO lease_token_audit;";

// v3: coalesced listener rejections; no per-request row on a flooded path.
const LEGACY_RATE_LIMIT_AUDIT_DDL: &str = "CREATE TABLE lease_listener_limit_audit (
         limit_key TEXT NOT NULL,
         limit_name TEXT NOT NULL,
         first_at TEXT NOT NULL,
         last_at TEXT NOT NULL,
         count INTEGER NOT NULL,
         PRIMARY KEY (limit_key, limit_name, first_at)
     );";

/// Bring the store to the newest schema this binary knows.
pub(super) fn migrate_lease_token_store(
    connection: &Connection,
    path: &Path,
) -> Result<(), OrbitError> {
    // Preserve the upgrade diagnostic for legacy stores with an unknown version.
    if ledger::current_schema_version(connection)? == 0 {
        legacy_schema_version(connection)?;
    }
    if let Some(forward) =
        ledger::run_database_migrations(connection, &DATABASE, MIGRATIONS, Some(path))?
    {
        return Err(newer_lease_token_store(
            i64::from(forward.state_version),
            i64::from(SUPPORTED_VERSION),
        ));
    }
    Ok(())
}

fn apply_lease_token_baseline(connection: &Connection) -> Result<(), OrbitError> {
    // Check under the ledger's write lock too, in case a pre-ledger binary
    // initialized the file between our first read and the transaction.
    let legacy_version = legacy_schema_version(connection)?;
    if legacy_version < 1 {
        connection
            .execute_batch(LEGACY_TOKENS_DDL)
            .map_err(sqlite_store_error)?;
    }
    if legacy_version < 2 {
        connection
            .execute_batch(LEGACY_LISTENER_AUDIT_DDL)
            .map_err(sqlite_store_error)?;
    }
    if legacy_version < LAST_LEGACY_VERSION {
        connection
            .execute_batch(LEGACY_RATE_LIMIT_AUDIT_DDL)
            .map_err(sqlite_store_error)?;
        // Leave a schema marker that older listener binaries still recognize.
        connection
            .pragma_update(None, "user_version", LAST_LEGACY_VERSION)
            .map_err(sqlite_store_error)?;
    }
    Ok(())
}

fn legacy_schema_version(connection: &Connection) -> Result<i64, OrbitError> {
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(sqlite_store_error)?;
    if !(0..=LAST_LEGACY_VERSION).contains(&version) {
        return Err(newer_lease_token_store(version, LAST_LEGACY_VERSION));
    }
    Ok(version)
}

fn newer_lease_token_store(version: i64, known: i64) -> OrbitError {
    OrbitError::Migration(format!(
        "the lease token store is at schema version {version}, newer than this Orbit \
         knows ({known}); update the listener user's orbit binary"
    ))
}
