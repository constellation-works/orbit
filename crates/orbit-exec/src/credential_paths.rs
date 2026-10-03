//! The well-known credential locations no confined worker child may read.
//!
//! One list serves every OS-level confinement so the platforms cannot drift:
//! the macOS SBPL compiler emits each entry as a read deny, the Linux
//! Bubblewrap plan hides each existing entry behind a mask mount, and a
//! brokered plugin backend carries the same paths as read exclusions
//! ([`default_credential_read_denies`]). Entries that only exist on one
//! platform (`~/Library/Keychains`) are harmless on the other: a path that is
//! absent is denied by SBPL and skipped by Bubblewrap.
//!
//! Delivery (commit, push, PR) and owner-task transport run in the unsandboxed
//! coordinator, never inside a confined worker, so denying these paths breaks
//! no legitimate worker flow. A worker reaches the owner without `~/.ssh` by
//! handing its result back through step output.

use std::ffi::OsStr;
use std::path::PathBuf;

/// HOME-relative path of the per-user keychain directory. Shared by the default
/// deny and the macOS provider re-allow so the two clauses cannot drift.
pub(crate) const USER_KEYCHAINS_SUBPATH: &str = "Library/Keychains";

/// HOME-relative credential trees, each denied as a whole subtree.
const HOME_CREDENTIAL_SUBPATHS: &[&str] = &[
    ".ssh",
    ".aws",
    ".config/gh",
    USER_KEYCHAINS_SUBPATH,
    "Library/Application Support/Google/Chrome",
    "Library/Application Support/Chromium",
    "Library/Application Support/BraveSoftware/Brave-Browser",
    "Library/Application Support/Firefox",
];

/// System-wide keychain trees (macOS).
const SYSTEM_CREDENTIAL_SUBPATHS: &[&str] = &["/Library/Keychains", "/System/Library/Keychains"];

/// Cargo credential file names, both spellings, denied for read.
const CARGO_CREDENTIAL_FILE_NAMES: &[&str] = &["credentials", "credentials.toml"];

/// One well-known credential location every confined child is denied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CredentialReadDeny {
    pub(crate) path: PathBuf,
    /// A single file beside granted siblings rather than a whole tree.
    pub(crate) file: bool,
}

/// The default credential read denies, resolved from `home` and `cargo_home`,
/// in the order the SBPL compiler emits them. One list serves the agent
/// profile on both platforms and a brokered plugin backend
/// ([`default_credential_read_denies`]), so they cannot drift.
pub(crate) fn credential_read_denies(
    home: Option<&OsStr>,
    cargo_home: Option<&OsStr>,
) -> Vec<CredentialReadDeny> {
    let mut denies = Vec::new();
    if let Some(home) = non_empty_env_path(home) {
        let home = home.display().to_string();
        for suffix in HOME_CREDENTIAL_SUBPATHS {
            denies.push(CredentialReadDeny {
                path: PathBuf::from(format!("{home}/{suffix}")),
                file: false,
            });
        }
    }

    // Cargo's crates.io publish token. It is a file at the `$CARGO_HOME` root
    // rather than inside a granted subdirectory, so it needs its own entry:
    // `registry`/`git` are writable while the token beside them is
    // unreadable. Both spellings are denied — cargo reads the legacy
    // extensionless `credentials` as well as `credentials.toml`. [ORB-12469]
    if let Some(cargo_home) = cargo_home_dir(home, cargo_home) {
        for name in CARGO_CREDENTIAL_FILE_NAMES {
            denies.push(CredentialReadDeny {
                path: PathBuf::from(format!("{}/{name}", cargo_home.display())),
                file: true,
            });
        }
    }

    for path in SYSTEM_CREDENTIAL_SUBPATHS {
        denies.push(CredentialReadDeny {
            path: PathBuf::from(path),
            file: false,
        });
    }
    denies
}

/// The well-known credential locations denied to every confined child,
/// resolved from this process's `HOME` and `CARGO_HOME`.
///
/// Exposed for a confinement that is not compiled from an agent profile — a
/// plugin backend the host spawns on an agent's behalf — so it carries the
/// same credential denies on either platform.
pub fn default_credential_read_denies() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME");
    let cargo_home = std::env::var_os("CARGO_HOME");
    credential_read_denies(home.as_deref(), cargo_home.as_deref())
        .into_iter()
        .map(|deny| deny.path)
        .collect()
}

/// Resolve Cargo's home directory the way cargo itself does: `$CARGO_HOME`
/// when the operator admitted it into the child environment, otherwise the
/// documented `$HOME/.cargo` default. `None` when neither resolves, in which
/// case no cargo entry is produced at all.
pub(crate) fn cargo_home_dir(home: Option<&OsStr>, cargo_home: Option<&OsStr>) -> Option<PathBuf> {
    non_empty_env_path(cargo_home)
        .or_else(|| non_empty_env_path(home).map(|path| path.join(".cargo")))
}

pub(crate) fn non_empty_env_path(value: Option<&OsStr>) -> Option<PathBuf> {
    let value = value?;
    if value.to_string_lossy().is_empty() {
        return None;
    }
    Some(PathBuf::from(value))
}
