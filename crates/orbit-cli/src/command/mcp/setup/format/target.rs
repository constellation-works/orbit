//! Where a client config write lands when the config path is a symlink.

use std::fs;
use std::path::{Path, PathBuf};

use orbit_core::OrbitError;

/// Longest symlink chain followed before the path is treated as a loop.
const MAX_LINK_HOPS: usize = 40;

/// The file a write to `path` must update.
///
/// A client config may be a symlink into a dotfiles repository. Atomic writes
/// rename over their final path component, which would replace such a link
/// with a regular file, so the write targets the file the link resolves to
/// instead. A dangling link resolves to the file it points at, which the write
/// then creates. A path that is not a symlink is returned unchanged.
pub(in crate::command::mcp::setup) fn resolve_config_target(
    path: &Path,
) -> Result<PathBuf, OrbitError> {
    let mut current = path.to_path_buf();
    for _ in 0..MAX_LINK_HOPS {
        let is_link = match fs::symlink_metadata(&current) {
            Ok(metadata) => metadata.file_type().is_symlink(),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => false,
            Err(err) => {
                return Err(OrbitError::Io(format!(
                    "failed to inspect '{}': {err}",
                    current.display()
                )));
            }
        };
        if !is_link {
            return Ok(current);
        }
        let link = fs::read_link(&current).map_err(|err| {
            OrbitError::Io(format!(
                "failed to read link '{}': {err}",
                current.display()
            ))
        })?;
        current = match current.parent() {
            Some(parent) => parent.join(link),
            None => link,
        };
    }
    Err(OrbitError::InvalidInput(format!(
        "too many levels of symbolic links resolving '{}'",
        path.display()
    )))
}
