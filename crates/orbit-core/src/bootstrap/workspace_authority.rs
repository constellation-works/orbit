//! Bind a workspace's layout upgrades to the generation authority that
//! protects its live clients.
//!
//! Generation admission is per authority root (`~/.orbit`, an explicit
//! `--root` or `ORBIT_ROOT`), but a workspace `.orbit` is discovered from the
//! working directory independently of it. A process under a fresh authority
//! could therefore discover a workspace whose clients participate in another
//! authority, find nobody live in its own, and advance the workspace layout
//! under them. The layout is the one workspace-scoped part of a compatibility
//! identity, so the guard covers exactly the open that would migrate it.
//!
//! The workspace records the canonical authority protecting it in
//! `state/generation-authority.json` (gitignored runtime state that older
//! binaries never read). A pending layout migration is admitted only under
//! that authority. A workspace that has recorded none is admitted under the
//! authority whose catalog registered the checkout, or the workspace data
//! directory itself when it is the explicit authority; any other authority is
//! refused before the bootstrap or the layout upgrade writes. A brand-new
//! workspace has no layout clients yet and records its creator.

use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::fs::generation::authority_root;
use orbit_store::workflow::layout::{SUPPORTED_LAYOUT_VERSION, current_layout_version};
use serde::{Deserialize, Serialize};

use crate::bootstrap::installed_binary::verify_installed_binary_keeps_state;
use crate::runtime::WorkspaceRuntimeBinding;

/// Record of the authority protecting a workspace, relative to its `.orbit`.
const AUTHORITY_RECORD: &str = "state/generation-authority.json";

#[derive(Debug, Serialize, Deserialize)]
struct AuthorityRecord {
    /// Canonical generation authority root whose participants use this
    /// workspace.
    authority: PathBuf,
}

/// Whether an admitted open should record its authority for the workspace.
#[must_use]
pub(crate) struct LayoutAuthorityAdmission {
    record: Option<PathBuf>,
}

/// Refuse a workspace layout upgrade that `global_root`'s generation authority
/// does not protect, or a layout or store schema migration the installed orbit
/// could not keep using, without writing anything.
///
/// Command surfaces call this before their own incidental writes; runtime
/// composition repeats it before bootstrap and records the authority.
pub fn verify_workspace_layout_authority(
    global_root: &Path,
    shared_root: &Path,
    binding: Option<&WorkspaceRuntimeBinding>,
) -> Result<(), OrbitError> {
    admit_workspace_layout_authority(global_root, shared_root, binding).map(|_| ())
}

pub(crate) fn admit_workspace_layout_authority(
    global_root: &Path,
    shared_root: &Path,
    binding: Option<&WorkspaceRuntimeBinding>,
) -> Result<LayoutAuthorityAdmission, OrbitError> {
    // A build that is not the installed orbit may not migrate past it, even
    // under the authority protecting the workspace [ORB-20262].
    verify_installed_binary_keeps_state(global_root, shared_root)?;
    let process = authority_root(global_root)?;
    let version = current_layout_version(shared_root)?;
    let pending = version > 0 && version < SUPPORTED_LAYOUT_VERSION;
    let admitted = LayoutAuthorityAdmission { record: None };
    match read_authority_record(shared_root) {
        Ok(Some(recorded)) => {
            if !pending || authority_root(&recorded)? == process {
                return Ok(admitted);
            }
            Err(refused(
                shared_root,
                version,
                format!(
                    "its clients are protected by generation authority '{}', not this \
                     process's '{}'",
                    recorded.display(),
                    process.display()
                ),
            ))
        }
        Ok(None) => {
            let registered = binding
                .and_then(|binding| binding.generation_authority.as_deref())
                .is_some_and(|authority| authority == process);
            let own_authority = shared_root
                .canonicalize()
                .is_ok_and(|shared| shared == process);
            if own_authority {
                // An explicit root is its own authority; recording it would
                // claim the workspace for every later open.
                return Ok(admitted);
            }
            if version == 0 || registered {
                return Ok(LayoutAuthorityAdmission {
                    record: Some(process),
                });
            }
            if !pending {
                return Ok(admitted);
            }
            Err(refused(
                shared_root,
                version,
                format!(
                    "it has recorded no generation authority and this process's authority \
                     '{}' has not registered it",
                    process.display()
                ),
            ))
        }
        Err(detail) if pending => Err(refused(shared_root, version, detail)),
        Err(_) => Ok(admitted),
    }
}

impl LayoutAuthorityAdmission {
    /// Record the admitted authority unless the workspace already names one.
    /// Best effort: a read-only workspace keeps opening, and a concurrent
    /// first open that recorded first wins.
    pub(crate) fn record(self, shared_root: &Path) {
        let Some(authority) = self.record else {
            return;
        };
        let content = match serde_json::to_string(&AuthorityRecord { authority }) {
            Ok(content) => content,
            Err(error) => {
                tracing::warn!(
                    target: "orbit.core.bootstrap",
                    error = %error,
                    "skipped workspace generation authority record"
                );
                return;
            }
        };
        let path = shared_root.join(AUTHORITY_RECORD);
        match orbit_common::fs::io::write_new_private_text(&path, &format!("{content}\n")) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => tracing::warn!(
                target: "orbit.core.bootstrap",
                path = %path.display(),
                error = %error,
                "skipped workspace generation authority record"
            ),
        }
    }
}

/// The recorded authority, `None` when the workspace has recorded none, and
/// an error detail when the record exists but cannot be used.
fn read_authority_record(shared_root: &Path) -> Result<Option<PathBuf>, String> {
    let path = shared_root.join(AUTHORITY_RECORD);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "its generation authority record '{}' is unreadable: {error}",
                path.display()
            ));
        }
    };
    match serde_json::from_str::<AuthorityRecord>(raw.trim()) {
        Ok(record) if record.authority.is_absolute() => Ok(Some(record.authority)),
        Ok(_) => Err(format!(
            "its generation authority record '{}' does not name an absolute path",
            path.display()
        )),
        Err(error) => Err(format!(
            "its generation authority record '{}' is malformed: {error}",
            path.display()
        )),
    }
}

fn refused(shared_root: &Path, version: u32, detail: impl std::fmt::Display) -> OrbitError {
    OrbitError::Execution(format!(
        "upgrade admission refused: workspace '{}' has a pending layout upgrade \
         (v{version} to v{SUPPORTED_LAYOUT_VERSION}) but {detail}; leave the installation and \
         stores unchanged. Run the command under the generation authority its clients use \
         (the same home directory, `--root` or ORBIT_ROOT), where admission waits for them to \
         quiesce. Do not delete admission files or layout markers",
        shared_root.display()
    ))
}
