//! The probe Orbit runs for an implementer's deferral [ORB-15516].
//!
//! A deferral is sanctioned only where the implementer's lane refuses the
//! nested sandbox its tests apply. Orbit shows that itself rather than
//! trusting a hand-written probe: it spawns the deferring sandbox's probe the
//! way it spawns the provider, under the sandbox the provider's executor
//! resolves for the step's `fs_profile` in the checkout, and records the exit
//! status and output.

use std::path::Path;

use orbit_exec::spawn_owned_supervised_cancellable;
use orbit_types::workflow::{DeferralProbe, NamespaceProbe};

use crate::activity_job::cli_runner::spawn::{
    prepare_sandbox_for_dispatch, spawn_child_with_optional_sandbox,
};
use crate::context::RuntimeHost;

/// Wall clock for one probe. Both probes exit at once when the kernel
/// answers; this only bounds a wedged spawn.
const PROBE_TIMEOUT_MS: u64 = 10_000;

/// Run `probe` in the implementer's lane for a host that explicitly supports
/// [`RuntimeHost::run_deferral_probe`].
pub fn run_deferral_probe<H: RuntimeHost + ?Sized>(
    host: &H,
    probe: &DeferralProbe,
    provider: Option<&str>,
    fs_profile: Option<&str>,
    cwd: &Path,
) -> NamespaceProbe {
    let (exit_code, output) = spawn_probe(host, probe, provider, fs_profile, cwd)
        .unwrap_or_else(|reason| (-1, format!("the probe could not run: {reason}")));
    NamespaceProbe {
        command: probe.command.to_string(),
        exit_code,
        output,
    }
}

fn spawn_probe<H: RuntimeHost + ?Sized>(
    host: &H,
    probe: &DeferralProbe,
    provider: Option<&str>,
    fs_profile: Option<&str>,
    cwd: &Path,
) -> Result<(i32, String), String> {
    let sandbox = match provider {
        Some(provider) => host
            .resolve_executor_sandbox(provider, fs_profile, Some(cwd))
            .map_err(|error| format!("resolve the lane's sandbox: {error}"))?,
        None => None,
    };
    let prepared = prepare_sandbox_for_dispatch(sandbox.as_ref())
        .map_err(|error| format!("prepare the lane's sandbox: {}", error.message))?;
    let (program, args) = probe
        .argv
        .split_first()
        .ok_or_else(|| "the probe names no program".to_string())?;
    let args = args
        .iter()
        .map(|arg| (*arg).to_string())
        .collect::<Vec<_>>();
    let spawned = spawn_child_with_optional_sandbox(
        program,
        &args,
        &host.agent_subprocess_environment(&[]),
        Some(cwd),
        prepared.effective,
        provider.unwrap_or_default(),
    )
    .map_err(|error| error.message)?;
    let outcome = spawn_owned_supervised_cancellable(
        || Ok(spawned.child),
        Some(PROBE_TIMEOUT_MS),
        Some(Vec::new()),
        None,
    )
    .map_err(|error| error.to_string())?;
    if outcome.timed_out {
        return Err(format!("it did not exit within {PROBE_TIMEOUT_MS} ms"));
    }
    let result = outcome.result;
    let output = format!("{}\n{}", result.stdout.trim(), result.stderr.trim());
    Ok((result.exit_code.unwrap_or(-1), output.trim().to_string()))
}
