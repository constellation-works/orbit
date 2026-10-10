//! The two Bubblewrap binaries a Linux executor may run.
//!
//! The distribution's `/usr/bin/bwrap` is root-owned by its package manager.
//! The bundled fallback that `orbit init` installs gets the same guarantee
//! from where it lives and who owns it: one root-owned file at a fixed path,
//! under root-owned directories that neither group nor others can write. The
//! sandboxed agent runs as the unprivileged invoking user, so it can neither
//! rewrite that file nor rename anything over it, whatever its write policy
//! binds. Nothing else is ever executed as the wrapper.

use super::*;

/// The distribution's Bubblewrap. Preferred whenever it advertises both
/// `--bind-fd` and `--ro-bind-fd`.
pub const HOST_BWRAP_PATH: &str = "/usr/bin/bwrap";

/// Fixed root-owned location of the Bubblewrap Orbit ships for hosts whose
/// own is missing or lacks either required descriptor-backed bind option.
pub const BUNDLED_BWRAP_PATH: &str = "/usr/local/libexec/orbit/bwrap";

/// Upstream Bubblewrap release the bundled binary is built from. Keep in step
/// with `BWRAP_VERSION` in `scripts/build-bundled-bwrap.sh`.
pub const BUNDLED_BWRAP_VERSION: &str = "0.12.0";

/// Which trusted Bubblewrap a probe selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BwrapSource {
    /// The distribution's [`HOST_BWRAP_PATH`].
    Host,
    /// Orbit's [`BUNDLED_BWRAP_PATH`].
    Bundled,
}

impl BwrapSource {
    /// Stable label for diagnostics and JSON reports.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::Bundled => "bundled",
        }
    }

    /// The fixed path this source executes.
    pub fn path(self) -> &'static str {
        match self {
            Self::Host => HOST_BWRAP_PATH,
            Self::Bundled => BUNDLED_BWRAP_PATH,
        }
    }
}

/// Resolve a plan's wrapper to a binary the sandboxed process cannot have
/// replaced, checked at the moment of the call.
pub(super) fn trusted_wrapper(wrapper: &str) -> Result<PathBuf, OrbitError> {
    trusted_wrapper_at(wrapper, Path::new(BUNDLED_BWRAP_PATH))
}

/// [`trusted_wrapper`] with the bundled location supplied, so the ownership
/// refusal can be exercised against files a test can create.
pub(super) fn trusted_wrapper_at(wrapper: &str, bundled: &Path) -> Result<PathBuf, OrbitError> {
    if wrapper == HOST_BWRAP_PATH {
        return Ok(PathBuf::from(HOST_BWRAP_PATH));
    }
    if Path::new(wrapper) == bundled {
        verify_root_owned_wrapper(bundled).map_err(|reason| {
            OrbitError::Execution(format!("refusing bundled Bubblewrap `{wrapper}`: {reason}"))
        })?;
        return Ok(bundled.to_path_buf());
    }
    Err(OrbitError::Execution(format!(
        "refusing untrusted Bubblewrap wrapper `{wrapper}`"
    )))
}

/// Refuse a wrapper binary that any unprivileged principal could have
/// written: the file itself, or any directory on its absolute path through
/// which it could be replaced. Symlinks are refused at every level rather
/// than followed, so the checked object is the one that is executed.
#[cfg(unix)]
pub(super) fn verify_root_owned_wrapper(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;

    if !path.is_absolute() {
        return Err(format!("{} is not an absolute path", path.display()));
    }
    let inspect = |candidate: &Path| {
        std::fs::symlink_metadata(candidate)
            .map_err(|error| format!("inspect {}: {error}", candidate.display()))
    };
    let metadata = inspect(path)?;
    if let Some(reason) = ownership_refusal(
        WrapperEntry::File,
        metadata.file_type().is_file(),
        metadata.uid(),
        metadata.mode(),
    ) {
        return Err(format!("{} {reason}", path.display()));
    }
    for ancestor in path.ancestors().skip(1) {
        let metadata = inspect(ancestor)?;
        if let Some(reason) = ownership_refusal(
            WrapperEntry::Directory,
            metadata.file_type().is_dir(),
            metadata.uid(),
            metadata.mode(),
        ) {
            return Err(format!("{} {reason}", ancestor.display()));
        }
    }
    Ok(())
}

#[cfg(not(unix))]
pub(super) fn verify_root_owned_wrapper(path: &Path) -> Result<(), String> {
    Err(format!(
        "{} cannot be ownership-checked on this platform",
        path.display()
    ))
}

/// What a path component of a trusted wrapper must be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WrapperEntry {
    File,
    Directory,
}

/// Why one path entry disqualifies the wrapper, if it does. Pure so the
/// combinations can be tested without root-owned fixtures.
pub(super) fn ownership_refusal(
    expected: WrapperEntry,
    kind_matches: bool,
    uid: u32,
    mode: u32,
) -> Option<&'static str> {
    if !kind_matches {
        return Some(match expected {
            WrapperEntry::File => "is not a regular file",
            WrapperEntry::Directory => "is not a real directory",
        });
    }
    if uid != 0 {
        return Some("is not owned by root");
    }
    if mode & 0o022 != 0 {
        return Some("is writable by group or others");
    }
    if expected == WrapperEntry::File {
        if mode & 0o6000 != 0 {
            return Some("is setuid or setgid");
        }
        if mode & 0o111 == 0 {
            return Some("is not executable");
        }
    }
    None
}
