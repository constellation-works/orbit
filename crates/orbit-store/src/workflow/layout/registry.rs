//! Append-only workspace-layout migration registry.

use std::path::Path;

use orbit_common::OrbitError;

use super::steps::{
    apply_archive_friction_tasks, apply_baseline_layout, remove_legacy_task_projections,
};
use crate::contracts::MigrationCompatibility;

/// Highest workspace-layout version this binary knows how to produce.
/// Bump together with a new [`LAYOUT_MIGRATIONS`] entry — never without one.
pub const SUPPORTED_LAYOUT_VERSION: u32 = 3;

/// One entry in the layout-migration registry.
pub(crate) struct LayoutMigration {
    pub(crate) version: u32,
    pub(crate) name: &'static str,
    /// One-line human description surfaced by `orbit migrate --dry-run`.
    pub(crate) description: &'static str,
    /// What this migration means for a binary that does not have it. See
    /// [`MigrationCompatibility`]; declare `Breaking` when in doubt.
    pub(crate) compat: MigrationCompatibility,
    /// Applies the migration to the workspace `.orbit` directory. MUST be
    /// idempotent or staged (write-new-then-swap): a crash between `apply`
    /// and the marker write re-runs it on the next open. Directories the
    /// migration expects may be absent (fresh or partially-populated
    /// workspaces) — treat "nothing to do" as success.
    pub(crate) apply: fn(&Path) -> Result<(), OrbitError>,
}

/// Stable ordered registry of workspace-layout migrations. Append-only:
/// never renumber or edit an entry that has shipped. Every breaking
/// `.orbit/` layout change REQUIRES an entry here (see `RELEASING.md`).
pub(crate) const LAYOUT_MIGRATIONS: &[LayoutMigration] = &[
    LayoutMigration {
        version: 1,
        name: "baseline",
        description: "adopt the versioned .orbit/ layout (records the current shape; changes nothing)",
        // Records the shape older binaries already produce.
        compat: MigrationCompatibility::Additive,
        apply: apply_baseline_layout,
    },
    LayoutMigration {
        version: 2,
        name: "archive-friction-tasks",
        description: "rewrite affected task records from status 'friction' to 'archived', preserving the task and its event history",
        // Rewrites a removed status into one every binary understands; a
        // binary without this migration reads the result correctly. It never
        // re-runs, so an older writer that still records `friction` leaves
        // tasks the newer binary cannot load: not write-safe.
        compat: MigrationCompatibility::ReadCompatible,
        apply: apply_archive_friction_tasks,
    },
    LayoutMigration {
        version: 3,
        name: "remove-task-checkout-projections",
        description: "remove verified legacy .orbit/tasks symlinks without following them or touching canonical task bundles",
        // ORB-11994/12078: binaries without this migration still read tasks
        // through the removed projections — and recreate them when they
        // write.
        compat: MigrationCompatibility::Breaking,
        apply: remove_legacy_task_projections,
    },
];

/// This binary's layout ledger as upgrade admission compares it.
pub(crate) fn layout_compatibility() -> orbit_common::fs::generation::LedgerCompatibility {
    orbit_common::fs::generation::LedgerCompatibility::from_registry(
        LAYOUT_MIGRATIONS
            .iter()
            .map(|m| (m.version, m.compat.is_write_safe(), !m.compat.is_breaking())),
    )
}
