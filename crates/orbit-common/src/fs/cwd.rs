//! Shared working-directory confinement for operator host tools.
//!
//! `orbit.agent.invoke` and `orbit.command.exec` both start an unsandboxed
//! subprocess from an explicit directory. Containment is what keeps "this
//! workspace authorized the call" true: a caller addressing workspace A must
//! not be able to point that subprocess at workspace B, `$HOME`, or anywhere
//! else the Orbit user can reach.

use std::path::{Path, PathBuf};

use crate::OrbitError;

/// Canonicalize `requested` and require it to stay inside `checkout` or one
/// of `extra_roots`.
///
/// The path must be absolute, exist, and be a directory. Symlinks are
/// resolved before the containment check, so a link planted inside the
/// checkout that points outside is refused. Relative paths are refused
/// rather than resolved against the caller.
pub fn confine_workspace_cwd(
    field: &str,
    requested: &str,
    workspace_id: &str,
    checkout: &Path,
    extra_roots: &[PathBuf],
) -> Result<PathBuf, OrbitError> {
    let requested = requested.trim();
    if requested.is_empty() {
        return Err(OrbitError::InvalidInput(format!("`{field}` is required")));
    }
    let path = Path::new(requested);
    if !path.is_absolute() {
        return Err(OrbitError::InvalidInput(format!(
            "`{field}` must be an absolute path; got '{requested}'"
        )));
    }

    let canonical = path.canonicalize().map_err(|error| {
        OrbitError::InvalidInput(format!("`{field}` '{requested}' is not readable: {error}"))
    })?;
    if !canonical.is_dir() {
        return Err(OrbitError::InvalidInput(format!(
            "`{field}` '{requested}' is not a directory"
        )));
    }

    let checkout = checkout.canonicalize().map_err(|error| {
        OrbitError::Execution(format!(
            "canonicalize workspace root '{}': {error}",
            checkout.display()
        ))
    })?;
    if is_inside(&checkout, &canonical) {
        return Ok(canonical);
    }
    for extra in extra_roots {
        let Ok(root) = extra.canonicalize() else {
            continue;
        };
        if root.is_dir() && is_inside(&root, &canonical) {
            return Ok(canonical);
        }
    }

    Err(OrbitError::InvalidInput(format!(
        "{field} '{requested}' is outside workspace '{workspace_id}' checkout '{}'",
        checkout.display()
    )))
}

fn is_inside(root: &Path, candidate: &Path) -> bool {
    candidate.starts_with(root)
}
