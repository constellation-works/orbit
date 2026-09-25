use std::path::Path;

use orbit_common::OrbitError;
use rusqlite::Connection;

use crate::contracts::{ForwardCompatibleOpen, StateComponent, evaluate_newer_state};

mod audit_events;
mod baseline;
mod feature;
mod friction;
mod host_registry;
mod introspect;
mod invocation;
mod job_runs;
mod learning;
mod ledger;
mod operation_mode;
mod plugins;
mod routine;
mod task_commit_journal;
mod task_reservations;
mod tools;

// Schema steps, referenced by the ordered `ledger::MIGRATIONS` registry.
use audit_events::{
    apply_audit_actor_alias_v2, apply_audit_actor_identity, apply_audit_machine_name_columns,
    apply_audit_plugin_grants, apply_audit_self_reported_actor, apply_invocation_audit_context,
    apply_trusted_mcp_audit_provenance,
};
use baseline::apply_baseline_schema;
use feature::apply_feature_schema_ledger;
use friction::apply_friction_records_schema;
use host_registry::{
    apply_host_registry_core, apply_hub_registry_metadata, apply_workspace_coordination_projections,
};
use introspect::table_exists;
use invocation::{apply_invocation_telemetry_columns, apply_invocations_ts_index};
use job_runs::{
    apply_execution_provenance, apply_flat_crew_model, apply_job_run_archive_stage,
    apply_job_runs_created_index,
};
use learning::{apply_learning_index_workspace_scope, apply_remove_native_learning_subsystem};
use operation_mode::apply_remove_operation_mode;
use plugins::{
    apply_plugin_archive_digest, apply_plugin_certified_orbit_version,
    apply_plugins_and_audit_plugin_provenance,
};
use routine::apply_routine_scheduler_schema;
use task_commit_journal::apply_task_commit_journal;
use task_reservations::apply_workspace_claim_scope;

// The ledger tests reach schema introspection through `super::super::*`.
#[cfg(test)]
use introspect::table_has_column;

pub use feature::{
    AppliedFeatureMigration, FeatureMigration, FeatureSchemaStatus, PendingFeatureMigration,
};
pub use ledger::{AppliedMigration, SUPPORTED_SCHEMA_VERSION};
pub(crate) use ledger::{applied_migrations, current_schema_version};

/// Bring the store database up to the newest supported schema version,
/// applying any pending versioned migrations (each transactional and
/// recorded in the `schema_meta` ledger).
///
/// A database whose recorded schema version is newer than this binary
/// supports is decided by its forward-compatibility record (ORB-12434):
/// `Ok(Some(..))` means the caller must hold the connection read-only, and
/// an error means the database must not be opened at all.
pub(crate) fn apply_schema(conn: &Connection) -> Result<Option<ForwardCompatibleOpen>, OrbitError> {
    ledger::run_migrations(conn, ledger::MIGRATIONS)
}

pub(crate) fn apply_schema_at_path(
    conn: &Connection,
    path: &Path,
) -> Result<Option<ForwardCompatibleOpen>, OrbitError> {
    ledger::run_migrations_at_path(conn, ledger::MIGRATIONS, path)
}

/// Registry metadata for one schema migration not yet recorded as applied,
/// as surfaced by `orbit migrate --dry-run` (ORB-10012).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingSchemaMigration {
    pub version: u32,
    pub name: &'static str,
}

/// Read-only view of a store database's migration ledger: the recorded
/// schema version plus the registry migrations still pending against it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaLedgerStatus {
    /// Schema version recorded in the ledger (0 for a fresh/pre-ledger or
    /// nonexistent database).
    pub current_version: u32,
    /// Registry migrations newer than `current_version`, in apply order.
    /// Empty when the database is current or newer than this binary
    /// (compare against [`SUPPORTED_SCHEMA_VERSION`] to distinguish).
    pub pending: Vec<PendingSchemaMigration>,
    /// Set when the database is newer than this binary supports but only by
    /// additive migrations, so it can still be opened read-only (ORB-12434).
    /// `None` also covers a newer database this binary must refuse — the
    /// open path carries that refusal and its reason.
    pub forward_compatible: Option<ForwardCompatibleOpen>,
}

/// Inspect the migration ledger of the store database at `db_path` without
/// opening it for writing — and therefore without triggering the automatic
/// migrations that [`crate::Store::open`] applies. A missing database reads
/// as version 0 with every registry migration pending (opening it would
/// create and migrate it). Powers `orbit migrate --dry-run`.
pub fn read_schema_ledger_status(db_path: &Path) -> Result<SchemaLedgerStatus, OrbitError> {
    let (current_version, forward_compatible) = if db_path.exists() {
        let conn = Connection::open_with_flags(
            db_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|e| {
            OrbitError::Store(format!(
                "cannot open store database '{}' read-only: {e}",
                db_path.display()
            ))
        })?;
        let current_version = current_schema_version(&conn)?;
        let forward_compatible = if current_version > SUPPORTED_SCHEMA_VERSION {
            ledger::read_compat_record(&conn)
                .and_then(|record| {
                    evaluate_newer_state(
                        StateComponent::StoreSchema,
                        current_version,
                        SUPPORTED_SCHEMA_VERSION,
                        record,
                    )
                })
                .ok()
        } else {
            None
        };
        (current_version, forward_compatible)
    } else {
        (0, None)
    };

    Ok(SchemaLedgerStatus {
        current_version,
        pending: pending_schema_migrations_after(current_version),
        forward_compatible,
    })
}

/// Registry migrations newer than `current_version`, in apply order. Lets
/// callers that already know the recorded version (e.g. via
/// [`crate::Store::schema_version`]) list what is pending without another
/// database open.
pub fn pending_schema_migrations_after(current_version: u32) -> Vec<PendingSchemaMigration> {
    ledger::MIGRATIONS
        .iter()
        .filter(|m| m.version > current_version)
        .map(|m| PendingSchemaMigration {
            version: m.version,
            name: m.name,
        })
        .collect()
}

#[cfg(test)]
mod tests;
