//! Operator-facing diagnostics for provider failures caused by the spawn
//! sandbox or the provider's own configuration.

use std::ffi::OsStr;
use std::path::Path;

use orbit_common::OrbitError;
use orbit_exec::{
    MacosLoginKeychainAccess, linux_bwrap_write_grant_diagnostic, macos_login_keychain_access,
};
use orbit_types::policy::ResolvedFsProfile;
use orbit_types::workflow::ExecutorSandboxKind;
use orbit_types::workflow::activity_job::Provider;

use super::super::dispatcher::ResolvedSandbox;

/// Turn a child-reported EROFS into a policy-owned denial when the failing
/// program included the attempted path in stderr. This runs after the real
/// Bubblewrap child exits, so it covers the production invocation boundary
/// rather than merely explaining a path supplied by a unit test.
pub(super) fn linux_bwrap_failed_write_diagnostic(
    profile: &ResolvedFsProfile,
    stderr: &[u8],
    cwd: Option<&Path>,
) -> Result<Option<String>, OrbitError> {
    let stderr = String::from_utf8_lossy(stderr);
    for line in stderr.lines().rev() {
        if !line.contains("Read-only file system") && !line.contains("EROFS") {
            continue;
        }
        for candidate in failed_write_path_candidates(line).into_iter().rev() {
            let path = Path::new(&candidate);
            let attempted = if path.is_absolute() {
                path.to_path_buf()
            } else if let Some(cwd) = cwd {
                cwd.join(path)
            } else {
                continue;
            };
            if let Some(diagnostic) = linux_bwrap_write_grant_diagnostic(profile, &attempted)? {
                return Ok(Some(format!(
                    "Orbit linux-bwrap policy denied the attempted write: {diagnostic}"
                )));
            }
        }
    }
    Ok(None)
}

fn failed_write_path_candidates(line: &str) -> Vec<String> {
    let mut candidates = Vec::new();
    for quote in ['\'', '"', '`'] {
        let mut remainder = line;
        while let Some(start) = remainder.find(quote) {
            let after_start = &remainder[start + quote.len_utf8()..];
            let Some(end) = after_start.find(quote) else {
                break;
            };
            let candidate = after_start[..end].trim();
            if !candidate.is_empty() {
                candidates.push(candidate.to_string());
            }
            remainder = &after_start[end + quote.len_utf8()..];
        }
    }

    // Coreutils quotes paths, but language runtimes often render
    // `...: /path: Read-only file system`. Keep a conservative token fallback
    // so those failures are attributable too.
    let prefix = line
        .split_once("Read-only file system")
        .or_else(|| line.split_once("EROFS"))
        .map_or(line, |(prefix, _)| prefix);
    if let Some(token) = prefix.split_whitespace().next_back() {
        let candidate = token
            .trim_matches(|character: char| {
                matches!(character, '\'' | '"' | '`' | ':' | '(' | ')' | '[' | ']')
            })
            .trim();
        if !candidate.is_empty() && !candidates.iter().any(|known| known == candidate) {
            candidates.push(candidate.to_string());
        }
    }
    candidates
}

/// Text a provider CLI emits when it cannot read its Keychain-backed login.
/// The CLI cannot tell "the item is gone" from "the item is unreadable", so
/// each vendor reports both as a login failure in its own wording.
fn keychain_auth_failure_marker(provider: &str) -> Option<&'static str> {
    match Provider::parse(provider).ok()? {
        Provider::Claude => Some("OAuth session expired"),
        Provider::Copilot => Some("No authentication information found"),
        // Matches both the documented quoted form (`run 'agent login' first`)
        // and the live cursor-agent 2026.09.10 wording (`run agent login first`).
        Provider::Cursor => Some("Authentication required. Please run"),
        _ => None,
    }
}

/// Text `sandbox-exec` writes when the kernel refuses to apply the compiled
/// profile at all. It is the wrapper's own failure, not the confined program's.
const MACOS_SANDBOX_APPLY_FAILURE_MARKER: &str = "sandbox_apply: Operation not permitted";

/// Name a `sandbox-exec` wrapper that could not apply Orbit's profile.
///
/// Darwin reports this as exit 71 (`EX_OSERR`) with
/// `sandbox-exec: sandbox_apply: Operation not permitted` on stderr, and it
/// happens whenever the Orbit process is itself already confined — nesting a
/// second `sandbox-exec` is refused — or lacks the entitlement to apply one.
/// The provider binary never starts, so every later diagnostic that reads
/// provider output has nothing to key on and the step would otherwise persist a
/// bare exit code. This is a host/executor condition shared by every provider;
/// it says nothing about the profile's *contents* or about any credential store.
/// [DANI-10509]
pub(super) fn macos_sandbox_apply_failure_diagnostic(
    provider: &str,
    sandbox: Option<&ResolvedSandbox>,
    exit_code: Option<i32>,
    stderr: &str,
) -> Option<String> {
    let sandbox = sandbox?;
    if sandbox.kind != ExecutorSandboxKind::MacosSandboxExec
        || exit_code != Some(71)
        || !stderr.contains(MACOS_SANDBOX_APPLY_FAILURE_MARKER)
    {
        return None;
    }

    Some(format!(
        "sandbox-exec could not apply Orbit's macOS sandbox profile \
         (`{MACOS_SANDBOX_APPLY_FAILURE_MARKER}`), so the `{provider}` CLI never started: the \
         Orbit process is already sandboxed — nested sandbox-exec is refused — or lacks the \
         entitlement to apply a profile. Run Orbit outside the enclosing sandbox, or set this \
         executor's `sandbox: off` to run the provider unconfined. `allow_fallback` does not \
         cover this case: it permits bare exec only when the trusted sandbox-exec binary is \
         missing."
    ))
}

/// Turn Copilot's rejected `--model` error into crew-scoped remediation.
///
/// The CLI's stderr names the unavailable id but does not identify the Orbit
/// configuration that supplied it. The resolved crew is still present in the
/// activity input, so join those facts before the generic exit-code path drops
/// the actionable context.
pub(super) fn copilot_model_unavailable_diagnostic(
    provider: &str,
    crew: &str,
    output: &str,
) -> Option<String> {
    if Provider::parse(provider).ok()? != Provider::Copilot || crew.trim().is_empty() {
        return None;
    }

    let (_, after_prefix) = output.split_once("Model \"")?;
    let (model, _) = after_prefix.split_once("\" from --model flag is not available")?;
    if model.is_empty() {
        return None;
    }

    Some(format!(
        "Copilot rejected model `{model}` supplied by crew `{crew}`. Start `copilot` and enter \
         `/model` to list the ids available to this account, then update \
         `crews.{crew}.model` and retry."
    ))
}

/// Distinguish a sandbox Keychain denial from a genuinely expired provider
/// login.
///
/// A macOS sandbox that hides `$HOME/Library/Keychains` makes a valid login
/// look expired, and the CLI's own message sends the operator to re-login —
/// which cannot help. Orbit compiled the profile, so it is the only layer that
/// knows whether the credential was actually reachable; say so next to the
/// provider's message instead of leaving the operator to guess.
///
/// The verdict comes from `orbit_exec::macos_login_keychain_access`, which
/// reads the same profile the kernel enforced. Only the `Allowed` case may
/// recommend re-authentication: the other cases are Orbit's own denial, which
/// no amount of re-logging in outside the sandbox will clear. [ORB-10931]
/// [ORB-12261]
pub(super) fn macos_keychain_auth_diagnostic(
    provider: &str,
    sandbox: Option<&ResolvedSandbox>,
    output: &str,
) -> Option<String> {
    let home = std::env::var_os("HOME");
    macos_keychain_auth_diagnostic_with(provider, sandbox, output, home.as_deref())
}

/// Test-friendly variant: callers pass HOME explicitly instead of reading
/// process-global state, which the compiler's carve-out also depends on.
// pub(crate) widened for tests/ layout under ORB-00225; test reaches via exposed surface.
pub(crate) fn macos_keychain_auth_diagnostic_with(
    provider: &str,
    sandbox: Option<&ResolvedSandbox>,
    output: &str,
    home: Option<&OsStr>,
) -> Option<String> {
    let sandbox = sandbox?;
    let marker = keychain_auth_failure_marker(provider)?;
    if sandbox.kind != ExecutorSandboxKind::MacosSandboxExec || !output.contains(marker) {
        return None;
    }
    match macos_login_keychain_access(provider, home, &sandbox.fs_profile) {
        // The provider does not keep credentials in the keychain, so this
        // failure has nothing to do with the sandbox's keychain deny.
        MacosLoginKeychainAccess::DeniedByDefaultPolicy => None,
        MacosLoginKeychainAccess::Allowed => Some(format!(
            "Orbit's macOS sandbox profile allows `$HOME/Library/Keychains` reads for provider \
             `{provider}`, so the stored credential was reachable and this is a real login \
             failure: re-authenticate the provider CLI outside Orbit, then retry."
        )),
        // The compiler emits the re-allow only when it can resolve HOME, so an
        // Orbit process started without a login environment still produces the
        // fake-expiry failure. That is a different fix from re-authenticating.
        MacosLoginKeychainAccess::HomeUnresolved => Some(format!(
            "HOME is unset for the Orbit process, so its macOS sandbox profile could not allow \
             `$HOME/Library/Keychains` reads for provider `{provider}`: the sandbox hid the \
             stored credential and the login is not necessarily expired. Start Orbit with a \
             login environment and retry before re-authenticating."
        )),
        MacosLoginKeychainAccess::DeniedByActivityRule { rule } => Some(format!(
            "The activity's fsProfile `{}` denies `$HOME/Library/Keychains` reads via rule \
             `{rule}`, which outranks the `{provider}` credential carve-out, so Orbit's macOS \
             sandbox hid the stored credential and the login is not necessarily expired. \
             Re-authenticating will not help: drop or narrow that denyRead rule, or run this \
             activity without the macOS sandbox.",
            sandbox.fs_profile.name
        )),
    }
}
