//! Versioned `.orbit/` upgrade surface behind `orbit migrate` [ORB-10012].
//!
//! Both migration ledgers auto-apply when a workspace opens — the SQLite
//! schema ledger inside `Store::open` (ORB-10003) and the workspace-layout
//! registry in the open pre-flight — so `orbit migrate` is the *explicit*
//! surface over the same machinery:
//!
//! - `orbit migrate --confirm` (apply): opens the workspace normally, which
//!   applies everything pending, then reports the resulting versions and what
//!   the open applied through `orbit_core::app::workspace::exec`.
//! - `orbit migrate` (the default) and `orbit migrate --dry-run`: inspect
//!   explicit resolved roots *without* opening the runtime (see
//!   [`migrate_dry_run_at`]) so pending migrations are listed instead of
//!   silently applied — the only way to see "pending" given auto-migration on
//!   open. Environment and workspace-catalog resolution belongs to the calling
//!   application.
//!
//! Both paths also report forward compatibility (ORB-12434): a workspace
//! newer than this binary is not automatically unusable, and
//! [`MigrateStatus::forward_compatible_only`] distinguishes "newer, opens"
//! from "newer, refuses", and [`MigrateStatus::forward_compatible_writable`]
//! whether it opens for writes too.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_store::maintenance::migration::{SUPPORTED_SCHEMA_VERSION, read_schema_ledger_status};
use orbit_store::migrate::layout::{self, SUPPORTED_LAYOUT_VERSION};

pub use orbit_core::app::workspace::MigrateStatus;

/// Dry-run inspection against explicit roots. Read-only: reads the layout
/// marker and the schema ledger (read-only SQLite open), never applies.
pub fn migrate_dry_run_at(
    global_root: &Path,
    orbit_dir: &Path,
) -> Result<MigrateStatus, OrbitError> {
    let audit_db = orbit_config::resolved_audit_db_path(
        &orbit_core::composition::composition_env(),
        &orbit_config::ConfigRoots::new(global_root, orbit_dir),
    )?;
    let ledger = read_schema_ledger_status(&audit_db)?;

    Ok(MigrateStatus {
        orbit_dir: orbit_dir.to_path_buf(),
        layout_version: layout::current_layout_version(orbit_dir)?,
        layout_supported: SUPPORTED_LAYOUT_VERSION,
        schema_version: ledger.current_version,
        schema_supported: SUPPORTED_SCHEMA_VERSION,
        binary_version: env!("CARGO_PKG_VERSION"),
        pending_layout: layout::pending_layout_migrations(orbit_dir)?,
        pending_schema: ledger.pending,
        applied_layout: Vec::new(),
        layout_forward_compatible: layout::layout_forward_compatible_open(orbit_dir)?,
        schema_forward_compatible: ledger.forward_compatible,
    })
}
