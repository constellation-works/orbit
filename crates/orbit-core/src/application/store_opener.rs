//! The port through which code below composition opens an Orbit database it
//! was not handed: the host store without a workspace runtime, or a fresh
//! handle a long-lived cache reopens when the file is replaced.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_store::Store;

type OpenFn = fn(&Path) -> Result<Store, OrbitError>;

/// Opens an Orbit SQLite database by path. Composition builds the only value
/// (`orbit_core::composition::store_opener`); code below it receives one.
#[derive(Clone, Copy)]
pub struct StoreOpener {
    open: OpenFn,
    open_read_only: OpenFn,
}

impl StoreOpener {
    pub(crate) const fn new(open: OpenFn, open_read_only: OpenFn) -> Self {
        Self {
            open,
            open_read_only,
        }
    }

    /// Open the database for writing, creating and migrating it when needed.
    pub fn open(&self, path: &Path) -> Result<Store, OrbitError> {
        (self.open)(path)
    }

    /// Open an existing database without creating, migrating or writing it.
    pub fn open_read_only(&self, path: &Path) -> Result<Store, OrbitError> {
        (self.open_read_only)(path)
    }
}

impl std::fmt::Debug for StoreOpener {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("StoreOpener")
    }
}
