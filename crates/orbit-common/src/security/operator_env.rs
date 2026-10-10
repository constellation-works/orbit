//! Operator credentials for processes no login shell starts [ORB-15154].
//!
//! `orbit clock tick` runs straight from launchd or a systemd user timer, so
//! it holds none of the variables a login shell exports (a dedicated worker
//! token, say) and every run it starts would lack them whatever
//! `execution.env.pass` says. The operator keeps those values in one
//! owner-only file beside the global `config.toml`; the tick admits from it
//! only the names the effective `execution.env.pass` lists. The unit file
//! never contains a secret.

use std::io::Read;
use std::path::{Path, PathBuf};

use crate::OrbitError;

/// File name of the clock environment file under the global Orbit root.
pub const CLOCK_ENV_FILE_NAME: &str = "clock.env";

/// Upper bound on the file size read; a credential file is a few lines.
const MAX_ENV_FILE_BYTES: u64 = 64 * 1024;

/// The clock environment file for a global Orbit root.
pub fn clock_env_file_path(global_root: &Path) -> PathBuf {
    global_root.join(CLOCK_ENV_FILE_NAME)
}

/// The `pass` entries the file at `path` holds a non-empty value for, parsed
/// without touching the process environment. `Ok(None)` when the file does not
/// exist. Fails closed for a file that is a symlink, not a regular file, owned
/// by another user, or readable by group or others.
pub fn read_env_file(
    path: &Path,
    pass: &[String],
) -> Result<Option<Vec<(String, String)>>, OrbitError> {
    let file = match crate::fs::io::open_read_only_no_follow(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(OrbitError::Io(format!(
                "cannot open {}: {error}",
                path.display()
            )));
        }
    };
    let metadata = file
        .metadata()
        .map_err(|error| OrbitError::Io(format!("cannot stat {}: {error}", path.display())))?;
    if !metadata.is_file() {
        return Err(OrbitError::InvalidInput(format!(
            "{} must be a regular file",
            path.display()
        )));
    }
    require_owner_only(path, &metadata)?;
    let mut text = String::new();
    file.take(MAX_ENV_FILE_BYTES)
        .read_to_string(&mut text)
        .map_err(|error| OrbitError::Io(format!("cannot read {}: {error}", path.display())))?;
    Ok(Some(parse_env_text(&text, pass)))
}

#[cfg(unix)]
fn require_owner_only(path: &Path, metadata: &std::fs::Metadata) -> Result<(), OrbitError> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let mode = metadata.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(OrbitError::InvalidInput(format!(
            "refusing {}: mode {mode:03o} lets other users read it; run `chmod 600 {}`",
            path.display(),
            path.display()
        )));
    }
    // SAFETY: `geteuid` has no preconditions and cannot fail.
    let euid = unsafe { libc::geteuid() };
    if metadata.uid() != euid {
        return Err(OrbitError::InvalidInput(format!(
            "refusing {}: it is not owned by the user running this process",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(not(unix))]
fn require_owner_only(_path: &Path, _metadata: &std::fs::Metadata) -> Result<(), OrbitError> {
    Ok(())
}

/// Parse `NAME=value` lines, keeping only names in `pass`.
///
/// Blank lines and `#` comments are skipped, as is a leading `export `. A value
/// may be wrapped in one pair of single or double quotes; nothing is expanded.
/// Empty values and `ORBIT_` names are dropped: the latter are never
/// credentials and the child-environment builder refuses the privileged ones
/// anyway. A repeated name keeps its last value.
pub fn parse_env_text(text: &str, pass: &[String]) -> Vec<(String, String)> {
    let mut parsed: Vec<(String, String)> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line).trim_start();
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        let name = name.trim();
        if name.starts_with("ORBIT_") || !pass.iter().any(|allowed| allowed == name) {
            continue;
        }
        let value = unquote(value.trim());
        parsed.retain(|(existing, _)| existing != name);
        if !value.is_empty() {
            parsed.push((name.to_string(), value.to_string()));
        }
    }
    parsed
}

fn unquote(value: &str) -> &str {
    for quote in ['"', '\''] {
        if let Some(inner) = value
            .strip_prefix(quote)
            .and_then(|rest| rest.strip_suffix(quote))
        {
            return inner;
        }
    }
    value
}

/// Load child-environment defaults for the `pass` names the clock environment
/// file holds and the process lacks. A non-empty ambient value wins.
///
/// This only reads the environment and is safe for multithreaded callers.
/// Callers must pass the returned values as child-environment data and report
/// only their names; no credential is exported into the ambient process.
pub fn load_clock_env(
    global_root: &Path,
    pass: &[String],
) -> Result<Vec<(String, String)>, OrbitError> {
    Ok(read_env_file(&clock_env_file_path(global_root), pass)?
        .unwrap_or_default()
        .into_iter()
        .filter(|(name, _)| std::env::var_os(name).is_none_or(|held| held.is_empty()))
        .collect())
}

/// Names `pass` lists that the clock environment file holds a value for, or
/// `None` when the file is absent. Used to report on the file without
/// supplying its values to children. A refused or unreadable file is an error.
pub fn clock_env_file_names(
    global_root: &Path,
    pass: &[String],
) -> Result<Option<Vec<String>>, OrbitError> {
    Ok(read_env_file(&clock_env_file_path(global_root), pass)?
        .map(|entries| entries.into_iter().map(|(name, _)| name).collect()))
}
