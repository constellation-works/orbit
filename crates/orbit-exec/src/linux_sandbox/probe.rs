use super::*;

pub(super) const TRUSTED_BWRAP_PATH: &str = "/usr/bin/bwrap";

pub fn bwrap_program_for_audit() -> &'static str {
    TRUSTED_BWRAP_PATH
}

pub fn bwrap_path() -> Option<PathBuf> {
    let path = Path::new(TRUSTED_BWRAP_PATH);
    (cfg!(target_os = "linux") && is_executable(path)).then(|| path.to_path_buf())
}

pub fn bwrap_unavailable_message() -> String {
    format!("trusted Bubblewrap not available at {TRUSTED_BWRAP_PATH}")
}

/// Probe the namespaces and mounts Orbit relies on rather than treating a
/// present binary as usable. The host network namespace is retained
/// explicitly with `--share-net`.
///
/// Memoised per process for settled host properties: the trusted binary is
/// absent, `--help` lacks `--bind-fd`, or the capability probe succeeded.
/// Re-probing those spawned two processes per dispatch. An outcome the host
/// could not execute, or a capability probe that ran and exited non-zero, is
/// returned but not remembered — user-namespace exhaustion and similar
/// resource pressure clear without a process restart.
pub fn probe_bwrap() -> BwrapProbeOutcome {
    static SETTLED: OnceLock<BwrapProbeOutcome> = OnceLock::new();
    probe_bwrap_with(&SETTLED, probe_bwrap_now)
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
    let Some(path) = bwrap_path() else {
        return BwrapProbeMemo::Settled(BwrapProbeOutcome {
            available: false,
            trusted_path: TRUSTED_BWRAP_PATH.to_string(),
            detail: bwrap_unavailable_message(),
        });
    };
    let trusted_path = path.display().to_string();
    let could_not_execute = |error: std::io::Error| {
        BwrapProbeMemo::Unsettled(BwrapProbeOutcome {
            available: false,
            trusted_path: trusted_path.clone(),
            detail: format!("Bubblewrap capability probe could not execute: {error}"),
        })
    };
    let help = match Command::new(&path).arg("--help").output() {
        Ok(output) => output,
        Err(error) => return could_not_execute(error),
    };
    if !String::from_utf8_lossy(&help.stdout).contains("--bind-fd") {
        return BwrapProbeMemo::Settled(BwrapProbeOutcome {
            available: false,
            trusted_path,
            detail: "Bubblewrap does not support the required --bind-fd object-authority mount"
                .to_string(),
        });
    }
    let args = base_namespace_args();
    let output = match Command::new(&path)
        .args(&args)
        .arg("--")
        .arg("/bin/true")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
    {
        Ok(output) => output,
        Err(error) => return could_not_execute(error),
    };
    if output.status.success() {
        return BwrapProbeMemo::Settled(BwrapProbeOutcome {
            available: true,
            trusted_path,
            detail: "capability probe succeeded".to_string(),
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
    })
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
