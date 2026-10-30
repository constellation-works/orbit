//! The host's installed Orbit executable and the state it can keep using.
//!
//! A build that is not the installed executable (a development, candidate or
//! QA build) shares the host's stores whenever it runs under the same home.
//! Before such a build applies a migration, upgrade admission asks the
//! installed executable what it supports, so the build cannot advance shared
//! state past it.

use std::path::{Path, PathBuf};

use super::handoff::{PROBE_TIMEOUT, Report, contract_report};
use super::identity::CompatibilityIdentity;

/// Environment variable naming the managed install directory, as the
/// installer reads it.
pub const INSTALL_DIR_ENV: &str = "ORBIT_INSTALL_DIR";

/// The managed install directory for this process: `$ORBIT_INSTALL_DIR`, else
/// `$HOME/.orbit/bin`.
pub fn managed_install_dir() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os(INSTALL_DIR_ENV).filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(explicit));
    }
    std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(|home| PathBuf::from(home).join(".orbit").join("bin"))
}

/// The `orbit` executable in the managed install directory, when one exists
/// and this process is not running it.
pub fn installed_executable_other_than_this_process() -> Option<PathBuf> {
    let installed = managed_install_dir()?.join(format!("orbit{}", std::env::consts::EXE_SUFFIX));
    let installed_metadata = std::fs::metadata(&installed).ok()?;
    if !installed_metadata.is_file() {
        return None;
    }
    let running = std::env::current_exe().ok()?;
    (!same_file(&running, &installed, &installed_metadata)).then_some(installed)
}

/// The compatibility `executable` reports through `update --contract`.
/// `None` when it does not answer in time or does not report one.
pub fn reported_compatibility(executable: &Path) -> Option<CompatibilityIdentity> {
    match contract_report(executable, PROBE_TIMEOUT) {
        Report::Answered(report) if report["admission_contract"] == super::GENERATION_CONTRACT => {
            serde_json::from_value(report["compatibility"].clone()).ok()
        }
        Report::Answered(_) | Report::Unanswered => None,
    }
}

#[cfg(unix)]
fn same_file(running: &Path, _installed: &Path, installed_metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(running).is_ok_and(|running| {
        (running.dev(), running.ino()) == (installed_metadata.dev(), installed_metadata.ino())
    })
}

#[cfg(not(unix))]
fn same_file(running: &Path, installed: &Path, _installed_metadata: &std::fs::Metadata) -> bool {
    match (running.canonicalize(), installed.canonicalize()) {
        (Ok(running), Ok(installed)) => running == installed,
        _ => running == installed,
    }
}
