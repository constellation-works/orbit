use orbit_common::storage::sqlite::sqlite_error;
use orbit_common::{OrbitError, StorageLayer};
use rusqlite::{Connection, params};

use super::{LedgeredDatabase, Migration};
use crate::contracts::{
    CompatibilityRecord, CompatibilityRefusal, ForwardCompatibleOpen, evaluate_newer_state,
};

/// `schema_meta` key carrying the forward-compatibility record (ORB-12434).
/// Deliberately outside the `migration.v<NNNN>` namespace the ledger scans.
pub(crate) const COMPAT_KEY: &str = "migration.compat";

/// Decide a database recorded newer than this binary supports. Reads only
/// the compatibility record a newer binary left behind; never writes.
pub fn evaluate_newer_database(
    conn: &Connection,
    database: &LedgeredDatabase,
    current: u32,
    supported: u32,
) -> Result<ForwardCompatibleOpen, OrbitError> {
    let record = match read_compat_record(conn) {
        Ok(record) => evaluate_newer_state(database.component, current, supported, record, true),
        Err(refusal) => Err(refusal),
    };
    match record {
        Ok(forward) if forward.writable => {
            orbit_common::tracing::info!(
                target: "orbit.store.sqlite",
                database = database.noun,
                schema_version = current,
                supported_version = supported,
                "opening a newer database whose newer migrations keep older writers safe; this binary applies no schema migration to it",
            );
            Ok(forward)
        }
        Ok(forward) => {
            orbit_common::tracing::warn!(
                target: "orbit.store.sqlite",
                database = database.noun,
                schema_version = current,
                supported_version = supported,
                "opening a newer database read-only; this binary applies no schema migration to it",
            );
            Ok(forward)
        }
        Err(refusal) => Err(newer_than_supported(database, current, supported, &refusal)),
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
pub(super) fn write_compat_record(
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
        sqlite_error(
            StorageLayer::Migration,
            &error,
            format!("failed to record schema compatibility metadata at v{version}: {error}"),
        )
    })?;
    Ok(())
}

fn newer_than_supported(
    database: &LedgeredDatabase,
    current: u32,
    supported: u32,
    refusal: &CompatibilityRefusal,
) -> OrbitError {
    let noun = database.noun;
    OrbitError::Migration(format!(
        "{noun} schema version {current} is newer than the newest version this \
        orbit binary supports ({supported}); {refusal}; upgrade orbit to open this database"
    ))
}
