//! The execution-environment check every listener-user command runs first.
//!
//! The listener user's authority holds only while no other user can change
//! the code it runs or the home it keeps tokens in. So a command refuses to
//! start as root, or when its executable, the running user's Orbit home, or
//! any directory above either can be written by a user other than root or the
//! running user. Without the check, an agent running as the operator could
//! replace the binary and run code as the listener user.
//!
//! "Writable" is judged from ownership and mode bits: an owner other than
//! root or the running user (an owner can always `chmod`), or a group- or
//! world-write bit. A sticky directory above the checked path (such as
//! `/tmp`) is accepted, since other users cannot rename or delete entries in
//! it that they do not own, and the entry below it is checked in turn. ACLs
//! and mandatory access control are not read.

use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;

use orbit_common::OrbitError;

/// Refuse unless the running user, its executable and `orbit_home` are safe
/// for listener-user authority. See the module documentation.
pub fn check_listener_execution_environment(orbit_home: &Path) -> Result<(), OrbitError> {
    platform::check(orbit_home)
}

/// The account the operator becomes to run listener-user commands, by
/// convention.
pub const LISTENER_USER: &str = "orbit-lease";

fn refusal(detail: String) -> OrbitError {
    OrbitError::PolicyDenied(format!(
        "refusing to run as the lease listener user ({LISTENER_USER} by convention): {detail}"
    ))
}

#[cfg(unix)]
/// The paths a check covers: `path` itself and every directory above it.
fn path_and_ancestors(path: &Path) -> Vec<PathBuf> {
    path.ancestors().map(Path::to_path_buf).collect()
}

#[cfg(unix)]
/// The existing path to check for `orbit_home`: the home itself, or its
/// nearest existing ancestor when it does not exist yet.
fn existing_prefix(path: &Path) -> Result<PathBuf, OrbitError> {
    for candidate in path.ancestors() {
        match candidate.canonicalize() {
            Ok(resolved) => return Ok(resolved),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(refusal(format!(
                    "cannot resolve {}: {error}",
                    candidate.display()
                )));
            }
        }
    }
    Err(refusal(format!(
        "{} has no existing ancestor",
        path.display()
    )))
}

#[cfg(unix)]
mod platform {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::path::Path;

    use orbit_common::OrbitError;

    use super::{existing_prefix, path_and_ancestors, refusal};

    const STICKY: u32 = 0o1000;

    pub(super) fn check(orbit_home: &Path) -> Result<(), OrbitError> {
        // SAFETY: `geteuid` has no preconditions and cannot fail.
        let euid = unsafe { libc::geteuid() };
        if euid == 0 {
            return Err(refusal(
                "running as root; run it as the listener user, for example \
                 `sudo -H -u orbit-lease orbit lease token …`"
                    .to_string(),
            ));
        }
        let executable = std::env::current_exe()
            .and_then(|path| path.canonicalize())
            .map_err(|error| refusal(format!("cannot resolve the running executable: {error}")))?;
        check_chain(&executable, euid, "executable")?;
        check_chain(&existing_prefix(orbit_home)?, euid, "Orbit home")
    }

    /// Check `leaf` and every directory above it. `leaf` is canonical, so no
    /// component is a symlink.
    fn check_chain(leaf: &Path, euid: u32, role: &str) -> Result<(), OrbitError> {
        for (depth, path) in path_and_ancestors(leaf).iter().enumerate() {
            let metadata = std::fs::metadata(path)
                .map_err(|error| refusal(format!("cannot inspect {}: {error}", path.display())))?;
            let owner = metadata.uid();
            if owner != 0 && owner != euid {
                return Err(refusal(format!(
                    "{} (on the path of the {role}) is owned by uid {owner}, a user other than \
                     root or the running user (uid {euid}), who can change it",
                    path.display()
                )));
            }
            let mode = metadata.permissions().mode();
            let shared_write = mode & 0o022 != 0;
            let sticky_ancestor = depth > 0 && metadata.is_dir() && mode & STICKY != 0;
            if shared_write && !sticky_ancestor {
                return Err(refusal(format!(
                    "{} (on the path of the {role}) is writable by a user other than root or \
                     the running user (mode {:03o}); remove group and other write access",
                    path.display(),
                    mode & 0o777
                )));
            }
        }
        Ok(())
    }
}

#[cfg(not(unix))]
mod platform {
    use std::path::Path;

    use orbit_common::OrbitError;

    use super::refusal;

    pub(super) fn check(_orbit_home: &Path) -> Result<(), OrbitError> {
        Err(refusal(
            "the lease listener user needs a Unix host".to_string(),
        ))
    }
}
