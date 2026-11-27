//! Workspace layout and SQLite schema migration status behind `orbit migrate`.

use std::path::PathBuf;

use orbit_common::OrbitError;
use orbit_store::contract::ForwardCompatibleOpen;
use orbit_store::maintenance::migration::{
    PendingSchemaMigration, SUPPORTED_SCHEMA_VERSION, pending_schema_migrations_after,
};
use orbit_store::migrate::layout::{self, LayoutMigrationInfo, SUPPORTED_LAYOUT_VERSION};

use crate::Core;

/// Version alignment between a workspace's on-disk state and this binary:
/// current vs supported versions for both ledgers, plus whatever is pending
/// (dry-run) or was just applied (apply path).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrateStatus {
    /// The workspace `.orbit` directory the status describes.
    pub orbit_dir: PathBuf,
    /// Workspace `.orbit/` layout version recorded in the marker (0 =
    /// pre-versioning workspace that has never been opened by this binary).
    pub layout_version: u32,
    /// Newest layout version this binary supports.
    pub layout_supported: u32,
    /// Store-database schema version recorded in the `schema_meta` ledger.
    pub schema_version: u32,
    /// Newest schema version this binary supports.
    pub schema_supported: u32,
    /// This orbit binary's package version.
    pub binary_version: &'static str,
    /// Layout migrations pending against the marker, in apply order.
    pub pending_layout: Vec<LayoutMigrationInfo>,
    /// Schema migrations pending against the ledger, in apply order.
    pub pending_schema: Vec<PendingSchemaMigration>,
    /// Layout migrations the current runtime open auto-applied (apply path
    /// only; always empty for a dry-run, which never applies).
    pub applied_layout: Vec<LayoutMigrationInfo>,
    /// Set when the workspace layout is newer than this binary supports but
    /// readable read-only (ORB-12434).
    pub layout_forward_compatible: Option<ForwardCompatibleOpen>,
    /// Set when the store database is newer than this binary supports but
    /// readable read-only (ORB-12434).
    pub schema_forward_compatible: Option<ForwardCompatibleOpen>,
}

impl MigrateStatus {
    /// Total migrations still pending across both ledgers.
    pub fn pending_total(&self) -> usize {
        self.pending_layout.len() + self.pending_schema.len()
    }

    /// True when either recorded version is newer than this binary supports
    /// (the workspace was written by a newer orbit).
    pub fn newer_than_binary(&self) -> bool {
        self.layout_version > self.layout_supported || self.schema_version > self.schema_supported
    }

    /// True when everything newer than this binary is newer only by
    /// migrations it can still read, so the workspace serves at least
    /// read-only commands (ORB-12434). False when nothing is newer.
    pub fn forward_compatible_only(&self) -> bool {
        if !self.newer_than_binary() {
            return false;
        }
        let layout_ok = self.layout_version <= self.layout_supported
            || self.layout_forward_compatible.is_some();
        let schema_ok = self.schema_version <= self.schema_supported
            || self.schema_forward_compatible.is_some();
        layout_ok && schema_ok
    }

    /// True when the workspace is [forward-compatible](Self::forward_compatible_only)
    /// and every newer migration keeps this binary's writes correct, so it
    /// reads and writes as usual.
    pub fn forward_compatible_writable(&self) -> bool {
        self.forward_compatible_only()
            && [
                self.layout_forward_compatible.as_ref(),
                self.schema_forward_compatible.as_ref(),
            ]
            .into_iter()
            .flatten()
            .all(|open| open.writable)
    }
}

pub(super) fn status(core: &Core) -> Result<MigrateStatus, OrbitError> {
    let runtime = core.runtime();
    let orbit_dir = runtime.shared_root();
    let schema_version = runtime.sqlite_store()?.schema_version()?;

    Ok(MigrateStatus {
        orbit_dir: orbit_dir.clone(),
        layout_version: layout::current_layout_version(&orbit_dir)?,
        layout_supported: SUPPORTED_LAYOUT_VERSION,
        schema_version,
        schema_supported: SUPPORTED_SCHEMA_VERSION,
        binary_version: env!("CARGO_PKG_VERSION"),
        pending_layout: layout::pending_layout_migrations(&orbit_dir)?,
        pending_schema: pending_schema_migrations_after(schema_version),
        applied_layout: runtime.layout_upgrade_report().applied.clone(),
        layout_forward_compatible: runtime.layout_upgrade_report().forward_compatible.clone(),
        schema_forward_compatible: runtime.sqlite_store()?.forward_compatible_open().cloned(),
    })
}
