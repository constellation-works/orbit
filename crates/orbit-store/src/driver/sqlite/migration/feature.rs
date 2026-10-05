//! Generic, namespaced schema migrations for vertical feature crates.
//!
//! Store owns the SQLite connection, writer serialization, and migration
//! ledger. A feature crate owns its migration callbacks and invokes
//! [`Store::apply_feature_migrations`] before exposing its persistence API.
//! Feature versions are independent of Store's global schema version.

use orbit_common::OrbitError;
use rusqlite::{Connection, TransactionBehavior, params};

use crate::Store;

const FEATURE_IDENTIFIER_MAX_BYTES: usize = 128;
const MIGRATION_NAME_MAX_BYTES: usize = 128;

/// One append-only migration in a feature-owned schema registry.
#[derive(Clone, Copy)]
pub struct FeatureMigration {
    version: u32,
    name: &'static str,
    apply: fn(&Connection) -> Result<(), OrbitError>,
}

impl FeatureMigration {
    /// Define one feature migration. Registries must start at version 1 and
    /// contain every version exactly once in ascending order.
    pub const fn new(
        version: u32,
        name: &'static str,
        apply: fn(&Connection) -> Result<(), OrbitError>,
    ) -> Self {
        Self {
            version,
            name,
            apply,
        }
    }

    pub fn version(self) -> u32 {
        self.version
    }

    pub fn name(self) -> &'static str {
        self.name
    }
}

/// One feature migration already committed to the shared SQLite database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedFeatureMigration {
    pub version: u32,
    pub name: String,
    pub applied_at: String,
}

/// One feature migration known by this binary but not yet applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingFeatureMigration {
    pub version: u32,
    pub name: String,
}

/// Validated view of one feature's independent schema ledger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeatureSchemaStatus {
    pub feature: String,
    pub current_version: u32,
    pub applied: Vec<AppliedFeatureMigration>,
    pub pending: Vec<PendingFeatureMigration>,
}

impl Store {
    /// Apply every pending migration for `feature` in order.
    ///
    /// Each migration callback and its ledger row commit in one `BEGIN
    /// IMMEDIATE` transaction. Earlier successful migrations remain committed
    /// when a later migration fails, while that failing migration leaves
    /// neither partial schema nor a ledger row. A newer on-disk feature schema,
    /// a gap, or a changed migration name fails closed before a callback runs.
    pub fn apply_feature_migrations(
        &self,
        feature: &str,
        migrations: &[FeatureMigration],
    ) -> Result<(), OrbitError> {
        validate_feature_identifier(feature)?;
        validate_registry(feature, migrations)?;

        // Avoid taking Store's serialized writer for migrations already
        // validated as applied. The transaction-local re-read below remains
        // authoritative and handles another process advancing the same
        // feature between this status read and lock acquisition.
        let current_version = self
            .feature_schema_status(feature, migrations)?
            .current_version;
        let pending_offset = usize::try_from(current_version).map_err(|error| {
            OrbitError::Migration(format!(
                "feature schema version {current_version} for '{feature}' cannot index this migration registry: {error}"
            ))
        })?;

        for migration in migrations.iter().skip(pending_offset) {
            let applied_now =
                self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
                let conn = tx.connection();
                let applied = read_applied_migrations(conn, feature)?;
                validate_applied_ledger(feature, migrations, &applied)?;

                let current_version = applied.last().map_or(0, |entry| entry.version);
                if current_version >= migration.version {
                    return Ok(false);
                }
                let expected = current_version.checked_add(1).ok_or_else(|| {
                    OrbitError::Migration(format!(
                        "feature schema version overflow for '{feature}'"
                    ))
                })?;
                if migration.version != expected {
                    return Err(OrbitError::Migration(format!(
                        "feature migration registry for '{feature}' cannot apply v{} after v{current_version}; expected v{expected}",
                        migration.version
                    )));
                }

                (migration.apply)(conn)?;
                conn.execute(
                    "INSERT INTO feature_schema_meta(feature, version, name, applied_at)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![
                        feature,
                        i64::from(migration.version),
                        migration.name,
                        crate::now_string(),
                    ],
                )
                .map_err(|error| {
                    OrbitError::Migration(format!(
                        "failed to record feature migration '{feature}' v{} ({}): {error}",
                        migration.version, migration.name
                    ))
                })?;
                Ok(true)
            })?;

            if applied_now {
                orbit_common::tracing::info!(
                    target: "orbit.store.sqlite",
                    feature,
                    version = migration.version,
                    name = migration.name,
                    "applied feature schema migration",
                );
            }
        }

        Ok(())
    }

    /// Inspect and validate one feature's schema ledger without mutating it.
    pub fn feature_schema_status(
        &self,
        feature: &str,
        migrations: &[FeatureMigration],
    ) -> Result<FeatureSchemaStatus, OrbitError> {
        validate_feature_identifier(feature)?;
        validate_registry(feature, migrations)?;
        self.with_read_connection(|conn| {
            let applied = read_applied_migrations(conn, feature)?;
            validate_applied_ledger(feature, migrations, &applied)?;
            let current_version = applied.last().map_or(0, |entry| entry.version);
            let pending = migrations
                .iter()
                .filter(|migration| migration.version > current_version)
                .map(|migration| PendingFeatureMigration {
                    version: migration.version,
                    name: migration.name.to_string(),
                })
                .collect();
            Ok(FeatureSchemaStatus {
                feature: feature.to_string(),
                current_version,
                applied,
                pending,
            })
        })
    }
}

fn read_applied_migrations(
    conn: &Connection,
    feature: &str,
) -> Result<Vec<AppliedFeatureMigration>, OrbitError> {
    let mut statement = conn
        .prepare(
            "SELECT version, name, applied_at
             FROM feature_schema_meta
             WHERE feature = ?1
             ORDER BY version ASC",
        )
        .map_err(|error| OrbitError::Store(error.to_string()))?;
    let rows = statement
        .query_map([feature], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(|error| OrbitError::Store(error.to_string()))?;

    let mut applied = Vec::new();
    for row in rows {
        let (version, name, applied_at) =
            row.map_err(|error| OrbitError::Store(error.to_string()))?;
        let version = u32::try_from(version).map_err(|error| {
            OrbitError::Migration(format!(
                "feature migration ledger for '{feature}' contains invalid version {version}: {error}"
            ))
        })?;
        applied.push(AppliedFeatureMigration {
            version,
            name,
            applied_at,
        });
    }
    Ok(applied)
}

fn validate_registry(feature: &str, migrations: &[FeatureMigration]) -> Result<(), OrbitError> {
    for (index, migration) in migrations.iter().enumerate() {
        let expected = u32::try_from(index)
            .ok()
            .and_then(|index| index.checked_add(1))
            .ok_or_else(|| {
                OrbitError::Migration(format!(
                    "feature migration registry for '{feature}' is too large"
                ))
            })?;
        if migration.version != expected {
            return Err(OrbitError::Migration(format!(
                "feature migration registry for '{feature}' is not contiguous: expected v{expected}, found v{} ({})",
                migration.version, migration.name
            )));
        }
        validate_migration_name(feature, migration.version, migration.name)?;
    }
    Ok(())
}

fn validate_applied_ledger(
    feature: &str,
    migrations: &[FeatureMigration],
    applied: &[AppliedFeatureMigration],
) -> Result<(), OrbitError> {
    for (index, entry) in applied.iter().enumerate() {
        let expected = u32::try_from(index)
            .ok()
            .and_then(|index| index.checked_add(1))
            .ok_or_else(|| {
                OrbitError::Migration(format!(
                    "feature migration ledger for '{feature}' is too large"
                ))
            })?;
        if entry.version != expected {
            return Err(OrbitError::Migration(format!(
                "feature migration ledger for '{feature}' is not contiguous: expected v{expected}, found v{}",
                entry.version
            )));
        }
        let Some(known) = migrations.get(index) else {
            let supported = migrations.last().map_or(0, |migration| migration.version);
            return Err(OrbitError::Migration(format!(
                "feature schema '{feature}' version {} is newer than the newest version this binary supports ({supported}); upgrade orbit to open this feature schema",
                entry.version
            )));
        };
        if entry.name != known.name {
            return Err(OrbitError::Migration(format!(
                "feature migration ledger for '{feature}' changed v{} from '{}' to '{}'; shipped migration names are immutable",
                entry.version, entry.name, known.name
            )));
        }
    }
    Ok(())
}

fn validate_feature_identifier(feature: &str) -> Result<(), OrbitError> {
    let valid = !feature.is_empty()
        && feature.len() <= FEATURE_IDENTIFIER_MAX_BYTES
        && feature.trim() == feature
        && feature
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'));
    if valid {
        return Ok(());
    }
    Err(OrbitError::Migration(format!(
        "invalid feature schema identifier '{feature}'; use 1-{FEATURE_IDENTIFIER_MAX_BYTES} ASCII letters, digits, '-', '_' or '.'"
    )))
}

fn validate_migration_name(feature: &str, version: u32, name: &str) -> Result<(), OrbitError> {
    if !name.is_empty()
        && name.len() <= MIGRATION_NAME_MAX_BYTES
        && name.trim() == name
        && !name.chars().any(char::is_control)
    {
        return Ok(());
    }
    Err(OrbitError::Migration(format!(
        "feature migration '{feature}' v{version} has an invalid name; names must be normalized, non-empty, control-free, and at most {MIGRATION_NAME_MAX_BYTES} bytes"
    )))
}

/// v9 `feature_schema_ledger` migration (ORB-10319): generic append-only
/// migration ledger for vertical feature crates. Feature versions advance
/// independently from the Store-global schema after this one-time ownership
/// boundary and are recorded transactionally with their feature-owned DDL.
pub(super) fn apply_feature_schema_ledger(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch(
        r#"
            CREATE TABLE IF NOT EXISTS feature_schema_meta (
                feature    TEXT NOT NULL,
                version    INTEGER NOT NULL,
                name       TEXT NOT NULL,
                applied_at TEXT NOT NULL,
                PRIMARY KEY(feature, version),
                CHECK (length(feature) > 0 AND feature = trim(feature)),
                CHECK (
                    typeof(version) = 'integer'
                    AND version > 0
                    AND version <= 4294967295
                ),
                CHECK (length(name) > 0 AND name = trim(name)),
                CHECK (length(applied_at) > 0)
            );

            CREATE TRIGGER IF NOT EXISTS feature_schema_meta_immutable_update
            BEFORE UPDATE ON feature_schema_meta
            BEGIN
                SELECT RAISE(ABORT, 'feature schema migration records are immutable');
            END;

            CREATE TRIGGER IF NOT EXISTS feature_schema_meta_immutable_delete
            BEFORE DELETE ON feature_schema_meta
            BEGIN
                SELECT RAISE(ABORT, 'feature schema migration records are append-only');
            END;
        "#,
    )
    .map_err(|error| OrbitError::Store(error.to_string()))
}
