//! Physical identity of a path whose tail may not exist yet.
//!
//! Every variant here answers the same question — where on disk does this
//! path land — by resolving the part that exists through the kernel and
//! re-appending the names that do not. Each answer feeds a containment
//! decision, and the variants differ exactly where those decisions differ, so
//! pick one by its contract rather than by which crate a caller already
//! imports:
//!
//! - [`physical_with_missing_tail`] — per-component canonicalization that
//!   applies `..` where the kernel would; infallible. Use when the answer is
//!   compared against roots resolved the same way (sandbox write grants).
//! - [`lexical_normalize`] — collapses `.` and `..` by name alone, never
//!   touching the filesystem. Use for comparisons that must not depend on what
//!   exists on this host.
//! - [`resolve_symlinks`] — follows *dangling* symlinks to their targets,
//!   bounded by the kernel's `ELOOP` limit, and fails closed. Use when a write
//!   through a dangling link would land at its target (policy rule matching,
//!   artifact source confinement).
//! - [`canonicalize_existing_prefix_checked`] — canonicalizes the deepest
//!   existing ancestor, surfaces any error other than `NotFound`, and answers
//!   `None` when it runs out of ancestors. Use when the caller must reject an
//!   unreadable or unresolvable path itself (Linux sandbox runtime paths).
//! - [`canonicalize_existing_prefix`] — the lenient walk: tries every parent,
//!   whatever error the nearer ones gave, and returns the input unchanged when
//!   none canonicalizes. Use for best-effort anchoring that a later check
//!   still bounds (task selector anchors).
//!
//! Only [`physical_with_missing_tail`] and [`lexical_normalize`] interpret
//! `..`, and only [`resolve_symlinks`] follows a dangling link; the others
//! treat both as ordinary missing names.

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

use crate::OrbitError;

/// The path as the kernel resolves it, for a path that need not exist.
///
/// Components are resolved left to right. Every component that exists is
/// canonicalized — symlinks followed — and the names that do not exist yet are
/// appended to that resolved prefix. A path that exists in full is therefore
/// its canonical path, and one whose tail is missing still lands under the
/// directory its existing ancestors physically live in, rather than under the
/// names they are spelled with.
///
/// A `..` is applied where the kernel would apply it: to the resolved prefix
/// when nothing is missing (so it follows a symlink before it climbs), and to
/// the last missing name otherwise (a directory that does not exist cannot be
/// a link, so the two cancel). No `..` is ever appended to a resolved prefix,
/// and a path whose `..` leaves the tail is still read under the physical
/// ancestor, never under its spelling. That matters wherever a spelling and a
/// physical path differ (macOS `/var` and `/tmp`, a symlinked home): a
/// containment check compares this answer against protected roots resolved the
/// same way, so a whole-path lexical fallback would let a traversal through a
/// missing directory slip past a root spelled through a symlink.
pub fn physical_with_missing_tail(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        match std::env::current_dir() {
            Ok(cwd) => cwd.join(path),
            Err(_) => return lexical_normalize(path),
        }
    };
    let mut resolved = PathBuf::new();
    let mut missing: Vec<OsString> = Vec::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => resolved.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if missing.pop().is_none() {
                    resolved.pop();
                }
            }
            Component::Normal(name) => {
                if missing.is_empty()
                    && let Ok(canonical) = resolved.join(name).canonicalize()
                {
                    resolved = canonical;
                } else {
                    missing.push(name.to_os_string());
                }
            }
        }
    }
    for name in missing {
        resolved.push(name);
    }
    resolved
}

/// Collapse `.` and `..` by name alone, without asking the filesystem.
///
/// Used for a path no existing ancestor can resolve, and for comparisons that
/// must not depend on what happens to exist on this host.
pub fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if out.parent().is_some() {
                    out.pop();
                }
            }
            other => out.push(other),
        }
    }
    out
}

/// Cap on symlink traversals while resolving a not-yet-existing tail. Mirrors
/// the kernel's `ELOOP` limit; a cycle of dangling links fails closed here
/// instead of looping.
const MAX_SYMLINK_FOLLOWS: usize = 40;

/// Resolve symlinks in `path`, tolerating a not-yet-existing tail (writes /
/// creates): canonicalize the nearest existing ancestor and rejoin the missing
/// components. [ORB-00418]
///
/// A *dangling* symlink reports `exists() == false` (`exists()` follows the
/// link), but an `O_CREAT` open through it creates the link's **target** — so a
/// dangling component is resolved via `read_link` rather than treated as a
/// missing tail. Without this, a dangling link inside an allowed subtree
/// pointing into a denied subtree would pass rule matching while the actual
/// write landed at the denied target.
///
/// Shared by `orbit-policy`'s rule matching and `orbit-tools`' workspace-boundary
/// check so the two enforcement layers cannot drift.
pub fn resolve_symlinks(path: &Path) -> Result<PathBuf, OrbitError> {
    resolve_symlinks_bounded(path.to_path_buf(), 0)
}

fn resolve_symlinks_bounded(path: PathBuf, follows: usize) -> Result<PathBuf, OrbitError> {
    if follows > MAX_SYMLINK_FOLLOWS {
        return Err(OrbitError::InvalidInput(format!(
            "too many levels of symbolic links resolving {}",
            path.display()
        )));
    }
    if path.exists() {
        return path
            .canonicalize()
            .map_err(|error| OrbitError::Io(format!("canonicalize {}: {error}", path.display())));
    }

    let mut missing = Vec::new();
    let mut ancestor = path.as_path();
    while !ancestor.exists() {
        // Dangling symlink: follow it (fail closed if the link is unreadable)
        // and restart resolution at the rejoined target.
        let is_symlink = ancestor
            .symlink_metadata()
            .map(|meta| meta.file_type().is_symlink())
            .unwrap_or(false);
        if is_symlink {
            let target = ancestor.read_link().map_err(|error| {
                OrbitError::Io(format!("read_link {}: {error}", ancestor.display()))
            })?;
            let mut rejoined = if target.is_absolute() {
                target
            } else {
                ancestor
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_default()
                    .join(target)
            };
            for name in missing.iter().rev() {
                rejoined.push(name);
            }
            return resolve_symlinks_bounded(rejoined, follows + 1);
        }

        let name = ancestor.file_name().ok_or_else(|| {
            OrbitError::InvalidInput(format!("path has no file name: {}", path.display()))
        })?;
        missing.push(name.to_os_string());
        ancestor = ancestor.parent().ok_or_else(|| {
            OrbitError::InvalidInput(format!("path has no existing parent: {}", path.display()))
        })?;
    }

    let mut resolved = ancestor
        .canonicalize()
        .map_err(|error| OrbitError::Io(format!("canonicalize {}: {error}", ancestor.display())))?;
    for name in missing.iter().rev() {
        resolved.push(name);
    }
    Ok(resolved)
}

/// Split a path into the deepest ancestor that already exists and the
/// components that do not, canonicalize that ancestor, and rejoin them.
///
/// Canonicalizing resolves every symlink on the existing part, which is what
/// makes the result usable as a containment decision: whatever the caller does
/// next happens at the real location, not at the name it was given. Only a
/// `NotFound` moves the walk to the parent; any other failure (permission,
/// a non-directory ancestor, a symlink loop) is returned as
/// [`OrbitError::Io`]. `Ok(None)` means the walk ran out of ancestors, or met
/// a component with no file name (such as `..`); callers phrase their own
/// rejection.
pub fn canonicalize_existing_prefix_checked(path: &Path) -> Result<Option<PathBuf>, OrbitError> {
    let mut existing = path.to_path_buf();
    let mut missing = Vec::<OsString>::new();

    let canonical_existing = loop {
        match existing.canonicalize() {
            Ok(canonical) => break canonical,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let Some(name) = existing.file_name() else {
                    return Ok(None);
                };
                missing.push(name.to_os_string());
                if !existing.pop() {
                    return Ok(None);
                }
            }
            Err(error) => {
                return Err(OrbitError::Io(format!(
                    "inspect path ancestor `{}`: {error}",
                    existing.display()
                )));
            }
        }
    };

    let mut resolved = canonical_existing;
    for component in missing.iter().rev() {
        resolved.push(component);
    }
    Ok(Some(resolved))
}

/// Canonicalize the longest existing ancestor, then append the missing tail.
///
/// Returns the original path if no ancestor can be canonicalized.
///
/// This is not a lenient wrapper of [`canonicalize_existing_prefix_checked`]:
/// it moves past *any* canonicalization error, not just `NotFound`, so a path
/// under a regular file or an unreadable directory still resolves against a
/// higher ancestor; and it skips a trailing `..` rather than stopping at it.
pub fn canonicalize_existing_prefix(path: &Path) -> PathBuf {
    if let Ok(canonical) = path.canonicalize() {
        return canonical;
    }

    let mut missing = Vec::new();
    let mut ancestor = path;
    while let Some(parent) = ancestor.parent() {
        if parent == ancestor {
            break;
        }
        if let Some(name) = ancestor.file_name() {
            missing.push(name.to_os_string());
        }
        if let Ok(canonical) = parent.canonicalize() {
            let mut resolved = canonical;
            for name in missing.iter().rev() {
                resolved.push(name);
            }
            return resolved;
        }
        ancestor = parent;
    }
    path.to_path_buf()
}
