//! The one place Orbit opens its SQLite store. A runtime receives the handle
//! composition opened; code that must open another database (the host store
//! without a workspace runtime, a replaced file, a diagnostic probe) receives
//! a [`StoreOpener`] built here.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_store::Store;

use crate::application::store_opener::StoreOpener;

/// The opener code below composition receives to open an Orbit database by
/// path.
pub fn store_opener() -> StoreOpener {
    StoreOpener::new(open_store, open_store_read_only)
}

#[allow(clippy::disallowed_methods)]
pub(crate) fn open_store(path: &Path) -> Result<Store, OrbitError> {
    Store::open(path)
}

#[allow(clippy::disallowed_methods)]
pub(crate) fn open_store_read_only(path: &Path) -> Result<Store, OrbitError> {
    Store::open_read_only(path)
}
