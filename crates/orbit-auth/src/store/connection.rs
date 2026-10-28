//! Opening and admitting the lease token store.

use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::storage::sqlite::open_private;

use crate::environment::{LISTENER_USER, check_listener_execution_environment};

use super::{LEASE_TOKEN_STORE_FILE, LeaseTokenStore, private_access, schema};

impl LeaseTokenStore {
    /// Run the execution-environment check, then open the store in the
    /// running user's Orbit home. Every listener-user surface opens it this
    /// way.
    pub fn open_for_listener_user() -> Result<Self, OrbitError> {
        let orbit_home = orbit_common::fs::path::global_orbit_dir()?;
        check_listener_execution_environment(&orbit_home)?;
        Self::open_in_orbit_home(&orbit_home)
    }

    /// Open the store in `orbit_home` without the execution-environment
    /// check; the home and store ownership and mode checks still apply.
    pub fn open_in_orbit_home(orbit_home: &Path) -> Result<Self, OrbitError> {
        let path = orbit_home.join(LEASE_TOKEN_STORE_FILE);
        match std::fs::symlink_metadata(&path) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(no_store_here(&path));
            }
            Err(error) => {
                return Err(OrbitError::Io(format!(
                    "cannot inspect {}: {error}",
                    path.display()
                )));
            }
        }
        private_access::require_owner_only(orbit_home, true)?;
        private_access::require_owner_only(&path, false)?;
        let opened = open_private(&path)?;
        if opened.read_only {
            return Err(OrbitError::PolicyDenied(format!(
                "the lease token store {} is read-only; it must be writable by the {LISTENER_USER} \
                 user",
                path.display()
            )));
        }
        let mut connection = opened.connection;
        schema::migrate_lease_token_store(&mut connection)?;
        Ok(Self { connection })
    }
}

fn no_store_here(path: &Path) -> OrbitError {
    OrbitError::PolicyDenied(format!(
        "no lease token store at {}: lease token commands run as the lease listener user \
         ({LISTENER_USER} by convention), whose Orbit home holds the store. Run them as that \
         user, for example `sudo -H -u {LISTENER_USER} orbit lease token list`. Setting up the \
         user and its store is described in the orbit-setup skill's remote-access reference.",
        PathBuf::from(path).display()
    ))
}
