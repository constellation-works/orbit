//! Application-owned bootstrap identity. This is not a workspace setting.
//!
//! Only Orbit is available in production. The alternate profile is deliberately
//! test-only until execution admission and child-process re-entry carry product
//! identity. A directory marker protects supported composition/bootstrap paths;
//! it is not an authorization boundary against raw filesystem/store access.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use orbit_common::OrbitError;

const PRODUCT_MARKER: &str = ".orbit-product";

static STAGING_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProductProfile {
    Orbit,
}

impl ProductProfile {
    pub(crate) fn identity(self) -> &'static str {
        match self {
            Self::Orbit => "orbit:v1\n",
        }
    }

    /// Read every supplied root before any caller starts generation admission,
    /// layout upgrades, forced deletion, config seeding, or reconciliation.
    pub(crate) fn validate_roots(self, roots: &[&Path]) -> Result<(), OrbitError> {
        for root in roots {
            self.validate_root(root)?;
        }
        Ok(())
    }

    fn validate_root(self, root: &Path) -> Result<(), OrbitError> {
        if self != Self::Orbit
            && (!root.is_absolute()
                || root
                    .components()
                    .any(|part| matches!(part, std::path::Component::ParentDir)))
        {
            return Err(OrbitError::InvalidInput(
                "alternate product roots must be explicit absolute paths without parent traversal"
                    .into(),
            ));
        }
        // Check physical ancestry as well as the selected directory: a fresh
        // child of another product root is not an independent storage root.
        for ancestor in root.ancestors() {
            match fs::canonicalize(ancestor) {
                Ok(physical) => {
                    for parent in physical.ancestors() {
                        self.validate_marker(parent)?;
                    }
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(OrbitError::Io(error.to_string())),
            }
        }
        // Existing Orbit roots predate product identity. Only Orbit may
        // adopt them. An alternate product must start empty or already
        // carry its own marker; it must never guess from filenames.
        if !self.validate_marker(root)? && self != Self::Orbit && root.exists() {
            let mut entries =
                fs::read_dir(root).map_err(|error| OrbitError::Io(error.to_string()))?;
            if entries
                .next()
                .transpose()
                .map_err(|error| OrbitError::Io(error.to_string()))?
                .is_some()
            {
                return Err(OrbitError::InvalidInput(format!(
                    "refusing unmarked nonempty product root: {}",
                    root.display()
                )));
            }
        }
        Ok(())
    }

    fn validate_marker(self, root: &Path) -> Result<bool, OrbitError> {
        let marker = root.join(PRODUCT_MARKER);
        match fs::symlink_metadata(&marker) {
            Ok(metadata) => {
                if !metadata.file_type().is_file() {
                    return Err(OrbitError::InvalidInput(format!(
                        "product marker must be a regular file: {}",
                        marker.display()
                    )));
                }
                let identity = fs::read_to_string(&marker)
                    .map_err(|error| OrbitError::Io(error.to_string()))?;
                if identity != self.identity() {
                    return Err(OrbitError::InvalidInput(format!(
                        "product ownership mismatch at {}: expected {}",
                        root.display(),
                        self.identity().trim()
                    )));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(OrbitError::Io(error.to_string())),
        }
        Ok(true)
    }

    /// Claim an explicitly initialized root, without replacing an existing
    /// marker. The public name appears only with the complete identity, and a
    /// failed publication removes its staging file instead of leaving an empty
    /// or partial marker. Composition-only opens validate without claiming;
    /// bootstrap initialization records ownership when the root is writable.
    pub(crate) fn claim_root(self, root: &Path) -> Result<(), OrbitError> {
        self.validate_root(root)?;
        orbit_common::fs::io::create_private_dir_all(root)
            .map_err(|error| OrbitError::Io(error.to_string()))?;
        // A peer may have published a complete marker while this root was
        // being created. Accept that identity; do not stage a replacement.
        if self.validate_marker(root)? {
            return Ok(());
        }
        let marker = root.join(PRODUCT_MARKER);
        match publish_complete_marker(&marker, self.identity()) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => self.validate_root(root),
            Err(error) => Err(OrbitError::Io(error.to_string())),
        }
    }
}

/// Install `identity` at `marker` without ever exposing a short write.
///
/// Bytes land in a sibling staging file and are fsynced before `hard_link`
/// adds the public directory entry. `hard_link` fails when that name is
/// already occupied — a regular file, a partial file, a directory, or a
/// symlink — so this does not replace a marker it did not publish. On every
/// exit the staging name is removed; the linked inode remains.
fn publish_complete_marker(marker: &Path, identity: &str) -> io::Result<()> {
    let staging = staging_path(marker)?;
    let _cleanup = StagingCleanup(&staging);
    {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staging)
            .map_err(|error| {
                if error.kind() == io::ErrorKind::AlreadyExists {
                    io::Error::other(format!(
                        "product marker staging name collided at {}",
                        staging.display()
                    ))
                } else {
                    error
                }
            })?;
        file.write_all(identity.as_bytes())?;
        file.sync_all()?;
    }
    fs::hard_link(&staging, marker)?;
    let parent = marker.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "product marker has no parent directory: {}",
                marker.display()
            ),
        )
    })?;
    let parent_dir = File::open(parent)?;
    orbit_common::fs::io::sync_parent_dir(&parent_dir)?;
    Ok(())
}

fn staging_path(marker: &Path) -> io::Result<PathBuf> {
    let file_name = marker.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("product marker path has no file name: {}", marker.display()),
        )
    })?;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let counter = STAGING_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut staging_name = std::ffi::OsString::from(".");
    staging_name.push(file_name);
    staging_name.push(format!(".{nanos}.{counter}.tmp"));
    Ok(marker.with_file_name(staging_name))
}

struct StagingCleanup<'a>(&'a Path);

impl Drop for StagingCleanup<'_> {
    fn drop(&mut self) {
        let _ = fs::remove_file(self.0);
    }
}
