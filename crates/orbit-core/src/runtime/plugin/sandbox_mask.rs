//! The agent sandbox's mask over plugin state and the plugin secret store
//! (design `docs/design/plugins/2_agent_call_broker.md` §6).
//!
//! A host that sandboxes an agent hides `<global_root>/state/plugins/` and
//! `<global_root>/state/plugin-secrets/` from it, for reads and writes. On
//! Linux Bubblewrap binds a read-only sentinel directory over each tree; on
//! macOS the agent profile denies both trees. Agent plugin calls reach the
//! backend through the run's broker instead, which runs on the host.
//!
//! A nested `orbit` recognizes the mask before it reads or spawns anything:
//! the sentinel file on Linux, a permission error on the tree on macOS. It
//! then never runs a plugin call in-process, and the secret store never reads
//! a masked tree as "nothing set".

use std::io;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;

use super::paths::{plugin_secret_store_dir, plugin_state_root};

/// The directory Bubblewrap binds over each masked tree, relative to the
/// global root. It sits in the broker's host-owned root, which no agent
/// profile grants writes to.
pub const PLUGIN_MASK_SENTINEL_DIR: &str = "state/plugin-broker/masked";
/// The one file the sentinel directory holds. Its presence at the top of a
/// plugin tree means the tree is the sentinel, not the host's directory.
pub const PLUGIN_MASK_SENTINEL_FILE: &str = ".orbit-brokered";

const SENTINEL_TEXT: &str = "Plugin state and secrets are not visible from an Orbit agent \
                             sandbox; plugin calls from the agent run on the host.\n";

/// The trees an agent sandbox masks.
pub fn plugin_masked_trees(global_root: &Path) -> [PathBuf; 2] {
    [
        plugin_state_root(global_root),
        plugin_secret_store_dir(global_root),
    ]
}

/// Whether this process runs inside an agent sandbox that masks plugin state
/// and secrets under `global_root`.
pub fn plugin_trees_masked(global_root: &Path) -> bool {
    plugin_masked_trees(global_root)
        .iter()
        .any(|tree| tree_masked(tree))
}

/// Whether `tree` is hidden from this process: the Linux sentinel stands in
/// for it, or the sandbox refuses to let this process look at it.
pub(crate) fn tree_masked(tree: &Path) -> bool {
    match std::fs::symlink_metadata(tree.join(PLUGIN_MASK_SENTINEL_FILE)) {
        Ok(_) => return true,
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => return true,
        Err(_) => {}
    }
    matches!(
        std::fs::symlink_metadata(tree),
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied
    )
}

/// The refusal for anything that would read or change `what` from inside a
/// masked sandbox.
pub(crate) fn not_visible(what: &str) -> OrbitError {
    OrbitError::PolicyDenied(format!(
        "{what} is not visible from an agent sandbox; run this on the host, outside the agent's \
         sandbox"
    ))
}

/// The paths a host masks for one sandboxed agent, created and resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedPluginMask {
    /// The read-only stand-in directory, holding [`PLUGIN_MASK_SENTINEL_FILE`].
    pub sentinel: PathBuf,
    /// The trees the agent must not reach, as physical paths.
    pub trees: Vec<PathBuf>,
}

/// Create both masked trees (`0700`) and the sentinel directory with its file,
/// then return their physical paths.
///
/// Bubblewrap can only mount over a path that already exists, so the host
/// creates what is missing before the agent starts. Every directory on the way
/// is created without following links and re-inspected: a symlink, a
/// non-directory or a directory another user owns is refused, because a mask
/// laid over a link would hide the link and leave its target readable.
#[cfg(unix)]
pub fn prepare_plugin_mask(global_root: &Path) -> Result<PreparedPluginMask, OrbitError> {
    let root = global_root.canonicalize().map_err(|error| {
        OrbitError::Execution(format!(
            "resolve the global root `{}` for the agent sandbox mask: {error}",
            global_root.display()
        ))
    })?;
    let mut trees = Vec::new();
    for tree in plugin_masked_trees(&root) {
        let relative = tree.strip_prefix(&root).map_err(|_| {
            OrbitError::Execution(format!(
                "masked tree `{}` is not beneath `{}`",
                tree.display(),
                root.display()
            ))
        })?;
        trees.push(create_owned_dir(&root, relative)?);
    }
    let sentinel = create_owned_dir(&root, Path::new(PLUGIN_MASK_SENTINEL_DIR))?;
    write_sentinel_file(&sentinel.join(PLUGIN_MASK_SENTINEL_FILE))?;
    Ok(PreparedPluginMask { sentinel, trees })
}

/// Create `root/relative` one component at a time with mode `0700`, refusing
/// any component that is a link, not a directory, or owned by another user.
#[cfg(unix)]
fn create_owned_dir(root: &Path, relative: &Path) -> Result<PathBuf, OrbitError> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};

    // SAFETY: `geteuid` only reads the calling process's credentials.
    let euid = unsafe { libc::geteuid() };
    let mut dir = root.to_path_buf();
    for component in relative.components() {
        dir.push(component);
        match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(mask_io_error("create", &dir, error)),
        }
        let metadata = std::fs::symlink_metadata(&dir)
            .map_err(|error| mask_io_error("inspect", &dir, error))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(OrbitError::PolicyDenied(format!(
                "the agent sandbox mask refuses `{}`: it is not a plain directory",
                dir.display()
            )));
        }
        if metadata.uid() != euid {
            return Err(OrbitError::PolicyDenied(format!(
                "the agent sandbox mask refuses `{}`: it is owned by uid {}, not this host \
                 (uid {euid})",
                dir.display(),
                metadata.uid()
            )));
        }
    }
    Ok(dir)
}

/// Create the sentinel file once, never through a link; an existing regular
/// file is kept.
#[cfg(unix)]
fn write_sentinel_file(path: &Path) -> Result<(), OrbitError> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o400)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(mut file) => file
            .write_all(SENTINEL_TEXT.as_bytes())
            .map_err(|error| mask_io_error("write", path, error)),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let metadata = std::fs::symlink_metadata(path)
                .map_err(|error| mask_io_error("inspect", path, error))?;
            if metadata.is_file() {
                Ok(())
            } else {
                Err(OrbitError::PolicyDenied(format!(
                    "the agent sandbox mask refuses `{}`: it is not a regular file",
                    path.display()
                )))
            }
        }
        Err(error) => Err(mask_io_error("create", path, error)),
    }
}

#[cfg(unix)]
fn mask_io_error(action: &str, path: &Path, error: io::Error) -> OrbitError {
    OrbitError::Execution(format!(
        "{action} `{}` for the agent sandbox mask: {error}",
        path.display()
    ))
}
