use super::*;
#[cfg(target_os = "linux")]
use std::os::unix::process::CommandExt;

use super::wrapper::{
    BUNDLED_BWRAP_PATH, BwrapSource, HOST_BWRAP_PATH, trusted_wrapper, verify_root_owned_wrapper,
};

/// The process's settled capability probe, shared by dispatch and the plan
/// compiler so a plan names the wrapper the probe selected.
static SETTLED: OnceLock<BwrapProbeOutcome> = OnceLock::new();

/// The wrapper a plan names: the binary the settled probe selected, or the
/// host's before any probe has settled. Never consults `PATH`.
pub fn bwrap_program_for_audit() -> &'static str {
    SETTLED
        .get()
        .and_then(|outcome| outcome.source)
        .unwrap_or(BwrapSource::Host)
        .path()
}

/// [`bwrap_program_for_audit`] when it is present and, for the bundled
/// binary, still passes the root-ownership check.
pub fn bwrap_path() -> Option<PathBuf> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    trusted_wrapper(bwrap_program_for_audit())
        .ok()
        .filter(|path| is_executable(path))
}

/// Probe the namespaces and mounts Orbit relies on rather than treating a
/// present binary as usable. The host network namespace is retained
/// explicitly with `--share-net`.
///
/// The host's `/usr/bin/bwrap` is selected whenever it advertises
/// `--bind-fd`. Only when it is missing or lacks that option is the bundled
/// binary considered, and only after it passes the root-ownership check.
///
/// Memoised per process only after a successful capability probe. A package
/// install or upgrade can make an absent or incompatible binary usable in a
/// long-lived process, so negative outcomes must be retried.
pub fn probe_bwrap() -> BwrapProbeOutcome {
    probe_bwrap_with(&SETTLED, probe_bwrap_now)
}

/// Recheck the host after onboarding changes without consulting dispatch's
/// successful-probe memo. This also reports the result for the current user,
/// not for the privileged package-manager subprocess.
pub fn probe_bwrap_fresh() -> BwrapProbeOutcome {
    match probe_bwrap_now() {
        BwrapProbeMemo::Settled(outcome) | BwrapProbeMemo::Unsettled(outcome) => outcome,
    }
}

/// Run the same fresh probe as the unprivileged account that will execute
/// Orbit after an installer was launched through sudo. The caller must supply
/// a non-root UID/GID from its trusted installation context.
#[cfg(target_os = "linux")]
pub fn probe_bwrap_fresh_for_user(uid: u32, gid: u32) -> BwrapProbeOutcome {
    match probe_bwrap_now_for(Some((uid, gid))) {
        BwrapProbeMemo::Settled(outcome) | BwrapProbeMemo::Unsettled(outcome) => outcome,
    }
}

#[cfg(target_os = "linux")]
fn drop_to_probe_user(command: &mut Command, uid: u32, gid: u32) {
    // CommandExt::uid/gid do not clear supplementary groups. Do all three
    // changes together in the child, before it executes bwrap, so a root-run
    // installer cannot get a false readiness pass through inherited groups.
    unsafe {
        command.pre_exec(move || {
            if libc::setgroups(0, std::ptr::null()) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::setgid(gid) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::setuid(uid) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

/// One probe attempt classified for the process-level memo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum BwrapProbeMemo {
    Settled(BwrapProbeOutcome),
    Unsettled(BwrapProbeOutcome),
}

/// Apply the memo policy to one probe attempt. Settled outcomes are stored in
/// `settled` and reused; unsettled outcomes are returned without storing so a
/// later call re-probes. Tests drive this seam with synthetic outcomes.
pub(super) fn probe_bwrap_with(
    settled: &OnceLock<BwrapProbeOutcome>,
    probe: impl FnOnce() -> BwrapProbeMemo,
) -> BwrapProbeOutcome {
    if let Some(outcome) = settled.get() {
        return outcome.clone();
    }
    match probe() {
        BwrapProbeMemo::Settled(outcome) => settled.get_or_init(|| outcome).clone(),
        BwrapProbeMemo::Unsettled(outcome) => outcome,
    }
}

/// One real probe. Unsettled results are a spawn the host refused, or a
/// capability probe that ran and exited non-zero — both are worth retrying.
fn probe_bwrap_now() -> BwrapProbeMemo {
    probe_bwrap_now_for(None)
}

fn probe_bwrap_now_for(identity: Option<(u32, u32)>) -> BwrapProbeMemo {
    let source = match select_wrapper(identity) {
        Ok(source) => source,
        Err(outcome) => return BwrapProbeMemo::Unsettled(outcome),
    };
    let trusted_path = source.path().to_string();
    let args = base_namespace_args();
    let mut capability_command = probe_command(source.path(), identity);
    let output = match capability_command
        .args(&args)
        .arg("--")
        .arg("/bin/true")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
    {
        Ok(output) => output,
        Err(error) => {
            return BwrapProbeMemo::Unsettled(could_not_execute(source, error));
        }
    };
    if output.status.success() {
        return BwrapProbeMemo::Settled(BwrapProbeOutcome {
            available: true,
            trusted_path,
            detail: "capability probe succeeded".to_string(),
            source: Some(source),
            version: wrapper_version(source.path(), identity),
        });
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let detail = stderr.trim();
    BwrapProbeMemo::Unsettled(BwrapProbeOutcome {
        available: false,
        trusted_path,
        detail: format!(
            "Bubblewrap capability probe failed{}{}",
            if detail.is_empty() { "" } else { ": " },
            detail
        ),
        source: Some(source),
        version: None,
    })
}

/// Pick the wrapper to capability-probe. A host binary that advertises
/// `--bind-fd` always wins; a host binary that cannot be inspected is
/// reported rather than bypassed.
fn select_wrapper(identity: Option<(u32, u32)>) -> Result<BwrapSource, BwrapProbeOutcome> {
    let unavailable = |trusted_path: &str, detail: String| BwrapProbeOutcome {
        available: false,
        trusted_path: trusted_path.to_string(),
        detail,
        source: None,
        version: None,
    };
    let host_problem = if cfg!(target_os = "linux") && is_executable(Path::new(HOST_BWRAP_PATH)) {
        match advertises_bind_fd(BwrapSource::Host, identity) {
            Ok(true) => return Ok(BwrapSource::Host),
            Ok(false) => format!("Bubblewrap at {HOST_BWRAP_PATH} {MISSING_BIND_FD}"),
            Err(outcome) => return Err(outcome),
        }
    } else {
        format!("trusted Bubblewrap not available at {HOST_BWRAP_PATH}")
    };
    let bundled = Path::new(BUNDLED_BWRAP_PATH);
    if !cfg!(target_os = "linux") || std::fs::symlink_metadata(bundled).is_err() {
        return Err(unavailable(
            HOST_BWRAP_PATH,
            format!(
                "{host_problem}, and no bundled Bubblewrap is installed at {BUNDLED_BWRAP_PATH}"
            ),
        ));
    }
    if let Err(reason) = verify_root_owned_wrapper(bundled) {
        return Err(unavailable(
            BUNDLED_BWRAP_PATH,
            format!("{host_problem}; refusing bundled Bubblewrap: {reason}"),
        ));
    }
    match advertises_bind_fd(BwrapSource::Bundled, identity) {
        Ok(true) => Ok(BwrapSource::Bundled),
        Ok(false) => Err(unavailable(
            BUNDLED_BWRAP_PATH,
            format!("{host_problem}; bundled Bubblewrap at {BUNDLED_BWRAP_PATH} {MISSING_BIND_FD}"),
        )),
        Err(outcome) => Err(outcome),
    }
}

/// Fixed wording `orbit init` keys on to install a capable wrapper.
const MISSING_BIND_FD: &str = "does not support the required --bind-fd object-authority mount";

fn advertises_bind_fd(
    source: BwrapSource,
    identity: Option<(u32, u32)>,
) -> Result<bool, BwrapProbeOutcome> {
    let help = probe_command(source.path(), identity)
        .arg("--help")
        .stdin(Stdio::null())
        .output()
        .map_err(|error| could_not_execute(source, error))?;
    Ok(help.status.success() && String::from_utf8_lossy(&help.stdout).contains("--bind-fd"))
}

/// `bubblewrap 0.12.0` → `0.12.0`. Diagnostic only; a missing version never
/// changes readiness.
fn wrapper_version(path: &str, identity: Option<(u32, u32)>) -> Option<String> {
    let output = probe_command(path, identity)
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .ok()
        .filter(|output| output.status.success())?;
    String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .last()
        .map(str::to_string)
}

fn probe_command(path: &str, identity: Option<(u32, u32)>) -> Command {
    let mut command = Command::new(path);
    #[cfg(target_os = "linux")]
    if let Some((uid, gid)) = identity {
        drop_to_probe_user(&mut command, uid, gid);
    }
    #[cfg(not(target_os = "linux"))]
    let _ = identity;
    command
}

fn could_not_execute(source: BwrapSource, error: std::io::Error) -> BwrapProbeOutcome {
    BwrapProbeOutcome {
        available: false,
        trusted_path: source.path().to_string(),
        detail: format!("Bubblewrap capability probe could not execute: {error}"),
        source: Some(source),
        version: None,
    }
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}
