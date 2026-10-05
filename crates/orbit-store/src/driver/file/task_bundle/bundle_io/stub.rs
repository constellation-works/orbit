//! Unpublished-stub detection and reaping for bundle directories that never
//! received a `task.yaml`.

use crate::driver::file::task_bundle::bundle_io::write::cleanup_partial_bundle;
use orbit_common::OrbitError;
use orbit_types::task::TASK_ENVELOPE_FILE_NAME;
use std::fs::{self};
use std::path::Path;

/// In-directory lock residue left by an aborted create. The live bundle lock
/// lives beside the directory; this name is only leftover residue.
const UNPUBLISHED_STUB_LOCK_FILE_NAME: &str = ".task.yaml.lock";

/// True when `bundle_dir` is a directory that never published `task.yaml` and
/// holds no canonical bundle content.
///
/// Aborted creates leave this residue — empty, or only a zero-byte
/// `.task.yaml.lock` — which is not a bundle and must not fail reindex or
/// listing of healthy neighbors. A present `task.yaml` that cannot be loaded
/// is corrupt and stays fail-closed. A directory missing `task.yaml` but
/// holding any other entry is unresolved data, not a stub.
///
/// Shared by reindex, listing, and `orbit doctor` so those surfaces cannot
/// disagree on what is reapable residue.
pub fn is_unpublished_stub(bundle_dir: &Path) -> bool {
    bundle_dir.is_dir()
        && !bundle_dir.join(TASK_ENVELOPE_FILE_NAME).is_file()
        && directory_holds_only_stub_residue(bundle_dir)
}

/// Empty, or containing only `.task.yaml.lock`. Any other name — sidecars,
/// `artifacts/`, leftover bytes — is bundle content. Unreadable directories
/// are not stubs (fail closed: do not reap).
fn directory_holds_only_stub_residue(bundle_dir: &Path) -> bool {
    let Ok(entries) = fs::read_dir(bundle_dir) else {
        return false;
    };
    for entry in entries {
        let Ok(entry) = entry else {
            return false;
        };
        if entry.file_name() != UNPUBLISHED_STUB_LOCK_FILE_NAME {
            return false;
        }
    }
    true
}

/// Remove an unpublished stub directory. No-op when `task.yaml` is present or
/// the directory holds any non-lock entry, so a healthy, merely corrupt, or
/// data-bearing partial bundle is never deleted this way.
pub(crate) fn reap_unpublished_stub(bundle_dir: &Path) -> Result<(), OrbitError> {
    if !is_unpublished_stub(bundle_dir) {
        return Ok(());
    }
    cleanup_partial_bundle(bundle_dir)
}
