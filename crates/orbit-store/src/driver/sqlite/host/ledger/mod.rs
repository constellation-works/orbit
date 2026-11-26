//! The host store's configuration for the shared SQLite ledger.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::storage::sqlite::ledger::{LedgeredDatabase, Migration, run_database_migrations};
use rusqlite::Connection;

use crate::contracts::{ForwardCompatibleOpen, StateComponent};

pub(super) const STORE_DATABASE: LedgeredDatabase = LedgeredDatabase {
    component: StateComponent::StoreSchema,
    noun: "store database",
    foreign_keys_off: false,
    stamp: None,
};

pub(super) fn run_store_migrations(
    conn: &Connection,
    migrations: &[Migration],
    path: Option<&Path>,
) -> Result<Option<ForwardCompatibleOpen>, OrbitError> {
    run_database_migrations(conn, &STORE_DATABASE, migrations, path)
}
