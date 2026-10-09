//! Versioned `.orbit/` workspace-layout migrations (ORB-10012, P3.4).
//!
//! The SQLite schema ledger ([`crate::driver::sqlite::migration`]) versions the store
//! *database*; this module versions everything else about a workspace's
//! on-disk `.orbit/` layout — directory structure, non-SQLite state files,
//! log/index locations. Layout migrations are an ordered, append-only
//! registry of `(version, name, description, apply)` entries mirroring the
//! schema ledger; the next breaking `.orbit/` change becomes a registry
//! entry instead of an undocumented break (see `RELEASING.md`).
//!
//! The current layout version is recorded in a plain-text marker file at
//! `<orbit_dir>/state/layout.version`. A marker file — not a `schema_meta`
//! row — because layout migrations may need to run *before* the store
//! database can open (a migration may move or restructure the database's own
//! location), and because reading one tiny file keeps the workspace-open
//! pre-flight cheap (no extra SQLite open on the hot path). A missing marker
//! means "pre-versioning workspace": all migrations run from the start, so
//! the v1 baseline (a no-op — version 1 *is* the current shape) adopts
//! existing workspaces exactly like the schema ledger's idempotent baseline
//! adopts legacy databases.
//!
//! Guarantees:
//! - **Auto-upgrade on open.** [`upgrade_workspace_layout`] runs as a
//!   pre-flight when a workspace opens (matching how the SQLite ledger
//!   auto-applies inside `Store::open`); `orbit migrate` is the explicit
//!   inspection/apply surface.
//! - **Forward-compatible open.** A marker newer than
//!   [`SUPPORTED_LAYOUT_VERSION`] is decided from the companion
//!   `state/layout.compat` record a newer binary leaves behind
//!   ([`crate::contracts::CompatibilityRecord`], ORB-12434): newer by
//!   additive migrations only opens (nothing is applied and the marker is
//!   never rewritten). The layout is not write-gated, so its additive
//!   migrations must keep older writers safe, and a read-compatible or
//!   breaking migration — or a missing, stale, or unreadable record — still
//!   refuses with [`OrbitError::Migration`](orbit_common::OrbitError::Migration), naming the first such migration
//!   this binary lacks. The SQLite ledger guards its database the same way.
//! - **Crash tolerance.** Every migration MUST be idempotent (or stage via
//!   write-new-then-swap): the marker is advanced (atomic temp-file +
//!   rename) only *after* a migration's `apply` returns, so a crash in
//!   between re-runs that migration on the next open. The marker itself is
//!   gitignored runtime state — a fresh clone of a migrated workspace simply
//!   re-runs the idempotent migrations and re-stamps.
//! - **Single upgrader.** Pending migrations apply under an advisory file
//!   lock (`state/layout.lock`) with a re-check of the marker after
//!   acquisition, so concurrent opens do not interleave migrations. The
//!   up-to-date fast path never touches the lock.

mod marker;
mod registry;
mod steps;
mod upgrade;

pub use registry::SUPPORTED_LAYOUT_VERSION;
pub(crate) use registry::layout_compatibility;
pub use upgrade::{
    LayoutMigrationInfo, LayoutUpgradeReport, current_layout_version,
    layout_forward_compatible_open, pending_layout_migrations, upgrade_workspace_layout,
};

#[cfg(test)]
mod tests;
