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
/// running image and supports `capability`. A candidate whose completed probe
/// refused is probed again only once the installation changes again; one that
/// did not answer in time is probed again at the next idle boundary.
pub fn handover_target(capability: Option<&str>) -> Option<PathBuf> {
    static REFUSED: Mutex<Option<RefusedKey>> = Mutex::new(None);
    let installed = replaced_installation()?;
    judge_candidate(&REFUSED, installed, capability, PROBE_TIMEOUT)
}

/// A refused candidate: its path, modification time and length.
type RefusedKey = (PathBuf, Option<SystemTime>, u64);

/// Probe `installed` unless `refused` already holds its answer. Only a
/// completed refusal is remembered: a probe that times out or cannot run says
/// nothing about the candidate (the first exec of a new image can be held
/// for tens of seconds while the OS assesses it), so the next call asks again.
pub(super) fn judge_candidate(
    refused: &Mutex<Option<RefusedKey>>,
    installed: PathBuf,
    capability: Option<&str>,
    timeout: Duration,
) -> Option<PathBuf> {
    let metadata = std::fs::metadata(&installed).ok()?;
    let key = (installed.clone(), metadata.modified().ok(), metadata.len());
    let mut refused = refused.lock().ok()?;
    if refused.as_ref() == Some(&key) {
        return None;
    }
    match HandoverCandidate::probe_within(&installed, timeout) {
        Probe::Unanswered => {
            tracing::warn!(
                target: "orbit.generation",
                installed = %installed.display(),
                "the installed Orbit executable did not describe its contract in time; \
                 asking again at the next idle boundary"
            );
            None
        }
        Probe::Answered(Some(candidate))
            if capability.is_none_or(|capability| candidate.resumes(capability)) =>
        {
            Some(installed)
        }
        Probe::Answered(_) => {
            tracing::warn!(
                target: "orbit.generation",
                installed = %installed.display(),
                "the installed Orbit executable cannot take over this process; it keeps \
                 running the replaced image until it exits"
            );
            *refused = Some(key);
            None
        }
    }
}

/// Whether the executable at `path` implements this admission protocol and,
/// when named, the resume `capability` the handover needs.
pub fn candidate_supports(path: &Path, capability: Option<&str>) -> bool {
    HandoverCandidate::probe(path)
        .is_some_and(|candidate| capability.is_none_or(|capability| candidate.resumes(capability)))
}

/// A candidate executable, as a long-lived process would judge it before
/// handing over: the resume capabilities its `update --contract` reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandoverCandidate {
    resume: Vec<String>,
}

impl HandoverCandidate {
    /// Ask the executable at `path` for its contract, as [`handover_target`]
    /// does. `None` when it does not answer within the probe timeout or does
    /// not implement this admission protocol.
    pub fn probe(path: &Path) -> Option<Self> {
        match Self::probe_within(path, PROBE_TIMEOUT) {
            Probe::Answered(candidate) => candidate,
            Probe::Unanswered => None,
        }
    }

    /// [`probe`](Self::probe), telling a candidate that answered (possibly
    /// refusing) from one that never did.
    fn probe_within(path: &Path, timeout: Duration) -> Probe {
        match contract_report(path, timeout) {
            Report::Unanswered => Probe::Unanswered,
            Report::Answered(report) => Probe::Answered(Self::from_report(&report)),
        }
    }

    fn from_report(report: &serde_json::Value) -> Option<Self> {
        if report["admission_contract"] != super::GENERATION_CONTRACT {
            return None;
        }
        let resume = report["resume"]
            .as_array()
            .map(|resume| {
                resume
                    .iter()
                    .filter_map(|entry| entry.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        Some(Self { resume })
    }

    /// A candidate already known to report `resume`.
    pub fn reporting(resume: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            resume: resume.into_iter().map(Into::into).collect(),
        }
    }

    /// Whether a process handing over with `capability` can resume on it.
    pub fn resumes(&self, capability: &str) -> bool {
        self.resume.iter().any(|entry| entry == capability)
    }
}

/// What probing a candidate established.
enum Probe {
    /// The candidate completed its answer: a candidate when it speaks this
    /// protocol, `None` when it refuses.
    Answered(Option<HandoverCandidate>),
    /// The candidate gave no answer: it could not be run, outlived the timeout
    /// or was killed.
    Unanswered,
}

enum Report {
    Answered(serde_json::Value),
    Unanswered,
}

fn contract_report(path: &Path, timeout: Duration) -> Report {
    let Ok(mut child) = Command::new(path)
        .args(["update", "--contract", "--json"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return Report::Unanswered;
    };
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Report::Unanswered;
            }
        }
    };
    // Death by signal is no answer; an exit code is, even a failing one: a
    // binary that predates `--contract` rejects the flag.
    if status.code().is_none() {
        return Report::Unanswered;
    }
    // A completed process that is not a successful contract report refuses.
    let refusal = Report::Answered(serde_json::Value::Null);
    if !status.success() {
        return refusal;
    }
    let Some(mut stdout) = child.stdout.take() else {
        return Report::Unanswered;
    };
    let mut raw = Vec::new();
    if std::io::Read::read_to_end(&mut std::io::Read::take(&mut stdout, 64 * 1024), &mut raw)
        .is_err()
    {
        return Report::Unanswered;
    }
    serde_json::from_slice(&raw).map_or(refusal, Report::Answered)
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
