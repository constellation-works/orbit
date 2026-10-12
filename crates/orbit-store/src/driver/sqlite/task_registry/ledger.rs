//! The task registry's own migration ledger, run by the store ledger engine.
//!
//! `tasks/index.sqlite` is a separate file from the store database, so it
//! carries its own `schema_meta` ledger rows and `migration.compat` record,
//! decided by the same rules: a newer registry whose newer entries are all
//! `Additive` stays writable, one with a `ReadCompatible` entry opens
//! read-only, and one with a `Breaking` entry is refused.
//!
//! # The `user_version` fence
//!
//! Binaries from before this ledger read only `PRAGMA user_version` and the
//! `task_action_keys.action_key` column: they accept exactly version 5 with
//! action keys, and recover a recognized version 6. The ledger therefore keeps
//! `user_version` as a fence for them, written in each migration's transaction
//! by [`write_user_version_fence`]:
//!
//! - [`PRE_LEDGER_COMPATIBLE`] (5) while every applied entry is `Additive`,
//!   so pre-ledger binaries keep reading and writing the registry;
//! - [`PRE_LEDGER_REFUSED`] (7) from the first non-`Additive` entry on, so they
//!   refuse it. They cannot read the compatibility record, so a
//!   `ReadCompatible` entry refuses them rather than holding them read-only.
//!   Never 6: pre-ledger binaries may recover a registry marked 6.
//!
//! Ledger-aware binaries ignore `user_version` once a registry has a ledger.

use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_error;
use orbit_common::{StorageLayer, fs::generation::LedgerCompatibility};
use rusqlite::Connection;

use super::baseline::apply_registry_baseline;
use crate::contracts::{MigrationCompatibility, StateComponent};
use crate::driver::sqlite::migration::{LedgeredDatabase, Migration};

/// `user_version` while every applied registry entry is `Additive`.
pub(super) const PRE_LEDGER_COMPATIBLE: u32 = 5;
/// `user_version` once any applied registry entry is not `Additive`.
pub(super) const PRE_LEDGER_REFUSED: u32 = 7;

/// Stable ordered registry of task-registry migrations. Append-only, with
/// frozen bodies, under the same rules as the store's `ledger::MIGRATIONS`:
/// never renumber or edit a shipped entry, and give each entry private
/// shape-defining code. Every prefix is guarded by the task registry schema
/// golden (`make goldens`).
pub(crate) const MIGRATIONS: &[Migration] = &[
    // The pre-ledger v5 format, its action keys and its envelope stamps. A
    // registry at `user_version` 5 or below, or the recognized 6, adopts the
    // ledger by running it: it is idempotent over every shipped shape.
    Migration {
        version: 1,
        name: "registry_baseline",
        compat: MigrationCompatibility::Additive,
        apply: apply_registry_baseline,
    },
];

/// Newest registry ledger version this binary produces.
pub(super) const SUPPORTED_REGISTRY_VERSION: u32 = MIGRATIONS[MIGRATIONS.len() - 1].version;

/// The task registry as the ledger engine runs it.
pub(crate) const REGISTRY_DATABASE: LedgeredDatabase = LedgeredDatabase {
    component: StateComponent::TaskRegistry,
    noun: "task registry",
    // The baseline's pre-v4 rewrite rebuilds `workspace_bindings`, the parent
    // of every other registry table.
    foreign_keys_off: true,
    stamp: Some(write_user_version_fence),
};

/// This binary's registry ledger as upgrade admission compares it.
pub(crate) fn registry_compatibility() -> LedgerCompatibility {
    LedgerCompatibility::from_registry(
        MIGRATIONS
            .iter()
            .map(|m| (m.version, m.compat.is_write_safe(), !m.compat.is_breaking())),
    )
}

/// Set the pre-ledger fence for a registry at `version` of `migrations`.
fn write_user_version_fence(
    conn: &Connection,
    migrations: &[Migration],
    version: u32,
) -> Result<(), OrbitError> {
    let fence = if migrations
        .iter()
        .filter(|m| m.version <= version)
        .all(|m| m.compat == MigrationCompatibility::Additive)
    {
        PRE_LEDGER_COMPATIBLE
    } else {
        PRE_LEDGER_REFUSED
    };
    conn.pragma_update(None, "user_version", i64::from(fence))
        .map_err(|e| {
            sqlite_error(
                StorageLayer::Migration,
                &e,
                format!("failed to set the task registry user_version fence: {e}"),
            )
        })
}
