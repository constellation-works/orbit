//! Handing a long-lived process over to a newly installed executable.
//!
//! `orbit mcp serve`, the dashboard and drain coordinators outlive upgrades.
//! At an idle boundary they ask [`replaced_installation`] whether the
//! installed executable still names their running image; when it does not,
//! and [`candidate_supports`] confirms the candidate speaks this protocol and
//! can resume the process's state, they replace their image with it through
//! [`reexec`]. Every Orbit descriptor is close-on-exec, so the generation and
//! registry locks are released by the exec itself and the new image joins
//! the authority like any newcomer.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use crate::OrbitError;

/// An `orbit mcp serve` process resumes its stdio session across the exec.
pub const RESUME_MCP_STDIO: &str = "mcp-stdio-v1";
/// A drain coordinator re-attaches to the run it was coordinating.
pub const RESUME_DRAIN_ADOPT: &str = "drain-adopt-v1";
/// Every resume capability this binary implements, as `update --contract`
/// reports them.
pub const RESUME_CAPABILITIES: &[&str] = &[RESUME_MCP_STDIO, RESUME_DRAIN_ADOPT];

/// How long the candidate may take to describe its contract.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// The installed executable, when it no longer names this running image.
///
/// `None` also when the installation is missing (mid-replacement, or
/// removed): there is nothing to hand over to yet.
pub fn replaced_installation() -> Option<PathBuf> {
    installed_if_replaced()
}

#[cfg(target_os = "linux")]
fn installed_if_replaced() -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;
    let link = std::fs::read_link("/proc/self/exe").ok()?;
    // An executable replaced by rename leaves its running inode unlinked,
    // which the kernel reports by suffixing the path.
    let installed = link
        .as_os_str()
        .as_bytes()
        .strip_suffix(b" (deleted)")
        .map_or(link.clone(), |path| PathBuf::from(OsStr::from_bytes(path)));
    let running = std::fs::metadata("/proc/self/exe").ok()?;
    let on_disk = std::fs::metadata(&installed).ok()?;
    ((running.dev(), running.ino()) != (on_disk.dev(), on_disk.ino())).then_some(installed)
}

#[cfg(target_os = "macos")]
fn installed_if_replaced() -> Option<PathBuf> {
    let installed = std::env::current_exe().ok()?;
    let mut file = std::fs::File::open(&installed).ok()?;
    super::image::verify_running_image(&mut file)
        .is_err()
        .then_some(installed)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn installed_if_replaced() -> Option<PathBuf> {
    None
}

/// The installed executable to hand this process over to: it replaced this
/// running image and supports `capability`. A candidate that refused is
/// probed again only once the installation changes again.
pub fn handover_target(capability: Option<&str>) -> Option<PathBuf> {
    static REFUSED: Mutex<Option<(PathBuf, Option<SystemTime>, u64)>> = Mutex::new(None);
    let installed = replaced_installation()?;
    let metadata = std::fs::metadata(&installed).ok()?;
    let key = (installed.clone(), metadata.modified().ok(), metadata.len());
    let mut refused = REFUSED.lock().ok()?;
    if refused.as_ref() == Some(&key) {
        return None;
    }
    if candidate_supports(&installed, capability) {
        return Some(installed);
    }
    tracing::warn!(
        target: "orbit.generation",
        installed = %installed.display(),
        "the installed Orbit executable cannot take over this process; it keeps running the \
         replaced image until it exits"
    );
    *refused = Some(key);
    None
}

/// Whether the executable at `path` implements this admission protocol and,
/// when named, the resume `capability` the handover needs.
pub fn candidate_supports(path: &Path, capability: Option<&str>) -> bool {
    let Some(report) = contract_report(path) else {
        return false;
    };
    report["admission_contract"] == super::GENERATION_CONTRACT
        && capability.is_none_or(|capability| {
            report["resume"]
                .as_array()
                .is_some_and(|resume| resume.iter().any(|entry| entry == capability))
        })
}

fn contract_report(path: &Path) -> Option<serde_json::Value> {
    let mut child = Command::new(path)
        .args(["update", "--contract", "--json"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + PROBE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let mut stdout = child.stdout.take()?;
    let mut raw = Vec::new();
    std::io::Read::read_to_end(&mut std::io::Read::take(&mut stdout, 64 * 1024), &mut raw).ok()?;
    serde_json::from_slice(&raw).ok()
}

/// Replace this process image with `executable`, running `args` with this
/// process's environment plus `env`. Returns only if the exec failed.
pub fn reexec(executable: &Path, args: &[OsString], env: &[(&str, &OsStr)]) -> OrbitError {
    let mut command = Command::new(executable);
    command.args(args);
    for (name, value) in env {
        command.env(name, value);
    }
    exec(command)
}

#[cfg(unix)]
fn exec(mut command: Command) -> OrbitError {
    use std::os::unix::process::CommandExt;
    OrbitError::Execution(format!(
        "could not hand over to the installed executable: {}",
        command.exec()
    ))
}

#[cfg(not(unix))]
fn exec(_command: Command) -> OrbitError {
    OrbitError::Execution("handing over to a new executable requires Unix exec".into())
}
