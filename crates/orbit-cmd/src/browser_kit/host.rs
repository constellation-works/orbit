//! What the kit commands need from the host: tools on PATH, the guard that
//! keeps them out of workers, and the child processes they run.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use orbit_common::OrbitError;
use orbit_common::process::{CapturedOutput, run_bounded_capped};
use orbit_common::security::child_env::MCP_MANAGED_CONTEXT_ENV;

/// The fix for a host without Node; Orbit never installs it.
pub(crate) const NODE_FIX: &str = "Install Node.js 18 or newer (https://nodejs.org or the system \
     package manager) so `node` and `npm` are on PATH, then rerun `orbit browser-kit install`. \
     Orbit does not install Node.";

/// Output retained from a probe.
const PROBE_OUTPUT_LIMIT: usize = 8 * 1024 * 1024;

/// The first executable `program` on this process's PATH.
pub(super) fn find_on_path(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|candidate| is_executable(candidate))
}

/// The first executable `program` in a colon-separated PATH value.
pub(crate) fn find_in_path_value(program: &str, path: &str) -> Option<PathBuf> {
    path.split(':')
        .filter(|entry| !entry.is_empty())
        .map(|entry| Path::new(entry).join(program))
        .find(|candidate| is_executable(candidate))
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// Refuse to change the kit from inside a worker or as root.
///
/// A worker cannot build the kit reliably (no `sudo`, a private `/tmp`), and
/// a kit built as root lands in root's home, where workers never look.
pub(super) fn require_host_operator() -> Result<(), OrbitError> {
    let set = |name: &str| std::env::var_os(name).is_some_and(|value| !value.is_empty());
    if set(MCP_MANAGED_CONTEXT_ENV) || set("ORBIT_WORKER_CONTEXT_REQUIRED") {
        return Err(OrbitError::InvalidInput(
            "the browser kit is host setup: run `orbit browser-kit` on the host as the user who \
             runs workers, never inside a worker"
                .to_string(),
        ));
    }
    #[cfg(unix)]
    if unsafe { libc::geteuid() } == 0 {
        return Err(OrbitError::InvalidInput(
            "run `orbit browser-kit` as the user who runs workers, not as root; \
             `install --with-deps` asks sudo for the system libraries itself"
                .to_string(),
        ));
    }
    Ok(())
}

pub(super) fn require_tool(program: &str) -> Result<PathBuf, OrbitError> {
    find_on_path(program)
        .ok_or_else(|| OrbitError::InvalidInput(format!("`{program}` is not on PATH. {NODE_FIX}")))
}

/// Run a step that legitimately takes minutes (npm, the browser download),
/// streaming its progress to stderr so stdout stays the command's record.
pub(super) fn run_streaming(command: &mut Command, what: &str) -> Result<(), OrbitError> {
    #[allow(
        clippy::disallowed_methods,
        reason = "an npm install or browser download streams progress to the operator's terminal and legitimately runs for minutes; no fixed deadline fits, and the operator can interrupt it"
    )]
    let status = command
        .stdin(Stdio::null())
        .stdout(Stdio::from(std::io::stderr()))
        .status()
        .map_err(|error| OrbitError::Execution(format!("start {what}: {error}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(OrbitError::Execution(format!(
            "{what} failed with {status}; see its output above"
        )))
    }
}

/// Run a step that may ask for the operator's password (`sudo`).
#[cfg(target_os = "linux")]
pub(super) fn run_interactive(command: &mut Command, what: &str) -> Result<(), OrbitError> {
    #[allow(
        clippy::disallowed_methods,
        reason = "interactive sudo authentication followed by a package install: it waits on a human at the terminal, so no deadline applies"
    )]
    let status = command
        .stdout(Stdio::from(std::io::stderr()))
        .status()
        .map_err(|error| OrbitError::Execution(format!("start {what}: {error}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(OrbitError::Execution(format!(
            "{what} failed with {status}; see its output above"
        )))
    }
}

/// Run a short probe with a deadline, capturing its output.
pub(super) fn run_probe(
    command: &mut Command,
    deadline: Duration,
) -> Result<CapturedOutput, OrbitError> {
    run_bounded_capped(command, deadline, PROBE_OUTPUT_LIMIT)
}

/// Quote `value` for a POSIX shell.
pub(super) fn sh_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}
