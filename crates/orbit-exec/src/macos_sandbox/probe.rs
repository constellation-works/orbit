//! Test capability checks; production dispatch keeps its fail-closed spawn path.

use std::io::Write;
use std::sync::OnceLock;

/// Result of applying a permissive Seatbelt profile to a harmless child.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SandboxExecApplyProbe {
    /// The child ran successfully under the profile.
    Applied,
    /// Seatbelt refused the apply operation (exit 71 with `sandbox_apply`).
    ApplyRefused(String),
    /// No trusted executable wrapper exists. This is not a skippable refusal.
    BinaryAbsent(String),
    /// Spawn, deadline, or an unexpected child failure. Tests must fail.
    Failed(String),
    /// Seatbelt is not used on this operating system; cross-platform tests run.
    NotMacos,
}

impl SandboxExecApplyProbe {
    /// Return whether a sandbox-dependent test should run, printing a named
    /// `SKIP:` notice directly to stderr for an apply refusal only.
    ///
    /// Panics for missing binaries, unexpected failures, or a refusal when
    /// `ORBIT_REQUIRE_SANDBOX_EXEC=1`. A skipped run is never host evidence.
    pub fn test_guard(&self, test: &str) -> bool {
        let required = std::env::var("ORBIT_REQUIRE_SANDBOX_EXEC").as_deref() == Ok("1");
        match self.guard_with_writer(test, required, &mut std::io::stderr()) {
            Ok(run) => run,
            Err(reason) => panic!("sandbox-exec test prerequisite failed: {test}: {reason}"),
        }
    }

    pub(super) fn guard_with_writer(
        &self,
        test: &str,
        required: bool,
        output: &mut impl Write,
    ) -> Result<bool, String> {
        match self {
            Self::Applied | Self::NotMacos => Ok(true),
            Self::ApplyRefused(reason) if !required => {
                let notice =
                    format!("SKIP: sandbox-exec cannot apply a profile: {test}: {reason}\n");
                output
                    .write_all(notice.as_bytes())
                    .map_err(|error| format!("write skip notice: {error}"))?;
                Ok(false)
            }
            Self::ApplyRefused(reason) => Err(format!("ORBIT_REQUIRE_SANDBOX_EXEC=1: {reason}")),
            Self::BinaryAbsent(reason) | Self::Failed(reason) => Err(reason.clone()),
        }
    }
}

/// Probe once per process, using a trusted absolute wrapper, a permissive
/// profile and `/usr/bin/true`. The child has a ten-second deadline and capped
/// output. This diagnostic does not change production sandbox behavior.
pub fn probe_sandbox_exec_apply() -> &'static SandboxExecApplyProbe {
    static PROBE: OnceLock<SandboxExecApplyProbe> = OnceLock::new();
    PROBE.get_or_init(probe_now)
}

/// Shared guard for tests that actually launch a Seatbelt-confined backend.
/// On other platforms this returns true without spawning a child.
pub fn macos_sandbox_test_guard(test: &str) -> bool {
    probe_sandbox_exec_apply().test_guard(test)
}

#[cfg(any(target_os = "macos", test))]
pub(super) struct ApplyResult {
    pub(super) success: bool,
    pub(super) exit_code: Option<i32>,
    pub(super) stderr: String,
}

#[cfg(any(target_os = "macos", test))]
pub(super) fn classify_apply_result(
    result: Option<Result<ApplyResult, String>>,
) -> SandboxExecApplyProbe {
    match result {
        None => {
            SandboxExecApplyProbe::BinaryAbsent(super::spawn::sandbox_exec_unavailable_message())
        }
        Some(Err(reason)) => SandboxExecApplyProbe::Failed(reason),
        Some(Ok(ApplyResult { success: true, .. })) => SandboxExecApplyProbe::Applied,
        Some(Ok(ApplyResult {
            exit_code: code,
            stderr,
            ..
        })) => {
            let reason = format!("permissive profile exited with {code:?}: {}", stderr.trim());
            if code == Some(71) && stderr.contains("sandbox_apply") {
                SandboxExecApplyProbe::ApplyRefused(reason)
            } else {
                SandboxExecApplyProbe::Failed(reason)
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn probe_now() -> SandboxExecApplyProbe {
    use orbit_common::process::run_bounded_capped;
    use std::process::Command;
    use std::time::Duration;

    let Some(path) = super::spawn::sandbox_exec_path() else {
        return classify_apply_result(None);
    };
    let result = run_bounded_capped(
        Command::new(path).args(["-p", "(version 1) (allow default)", "/usr/bin/true"]),
        Duration::from_secs(10),
        64 * 1024,
    )
    .map(|output| ApplyResult {
        success: output.status.success(),
        exit_code: output.status.code(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
    .map_err(|error| error.to_string());
    classify_apply_result(Some(result))
}

#[cfg(not(target_os = "macos"))]
fn probe_now() -> SandboxExecApplyProbe {
    SandboxExecApplyProbe::NotMacos
}
