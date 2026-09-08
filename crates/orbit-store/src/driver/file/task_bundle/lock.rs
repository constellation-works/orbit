use std::path::{Path, PathBuf};

/// Stable identity outside the removable bundle. Never unlink these lock files:
/// queued flock callers must acquire the same inode after a deletion. Keeping
/// the lock in the existing parent also lets a first reader coordinate with a
/// first writer before any auxiliary directories have been created.
pub(crate) fn bundle_lock_target(bundle_dir: &Path) -> PathBuf {
    bundle_dir.with_extension("bundle")
}
