//! The token store's own schema and its forward-only migrations.
//!
//! The store is a separate SQLite file, not a table in Orbit's main store, so
//! it carries its own version in `PRAGMA user_version`. Each entry of
//! [`MIGRATIONS`] moves the schema up one version; none is ever edited or
//! removed once shipped, and a store from a newer Orbit is refused rather
//! than downgraded.

use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::{Connection, TransactionBehavior};

/// Schema versions in order; version N is reached by applying entry N-1.
const MIGRATIONS: &[&str] = &[
    // v1: tokens (hash only, never the secret) and their audit trail.
    "CREATE TABLE lease_tokens (
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
     );",
    // v2: listener refusals, with the remote address and route of the request.
    "CREATE TABLE lease_token_audit_v2 (
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
     ALTER TABLE lease_token_audit_v2 RENAME TO lease_token_audit;",
];

/// Bring the store to the newest schema this binary knows.
pub(super) fn migrate_lease_token_store(connection: &mut Connection) -> Result<(), OrbitError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sqlite_store_error)?;
    let version: i64 = transaction
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(sqlite_store_error)?;
    let known = MIGRATIONS.len();
    let current = usize::try_from(version).unwrap_or(usize::MAX);
    if current > known {
        return Err(OrbitError::Migration(format!(
            "the lease token store is at schema version {version}, newer than this Orbit \
             knows ({known}); update the listener user's orbit binary"
        )));
    }
    for (index, statements) in MIGRATIONS.iter().enumerate().skip(current) {
        transaction
            .execute_batch(statements)
            .map_err(sqlite_store_error)?;
        transaction
            .pragma_update(None, "user_version", index + 1)
            .map_err(sqlite_store_error)?;
    }
    transaction.commit().map_err(sqlite_store_error)
}
