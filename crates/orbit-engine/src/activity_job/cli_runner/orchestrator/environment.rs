//! Provider credentials, deadlines and child environment composition.
use super::super::super::dispatcher::DispatchError;
use super::super::spawn::{CODEX_CA_CERTIFICATE_ENV, SSL_CERT_FILE_ENV};
use crate::context::RuntimeHost;
use orbit_common::security::child_env::ACTIVITY_DEADLINE_ENV;
use orbit_types::workflow::ExecutorSandboxKind;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Credential variables that keep `claude` off the Desktop app's shared login.
const CLAUDE_WORKER_CREDENTIAL_ENV: [&str; 2] = ["CLAUDE_CODE_OAUTH_TOKEN", "ANTHROPIC_API_KEY"];

/// The provider's wall-clock deadline, as the `ORBIT_ACTIVITY_DEADLINE_UNIX_MS`
/// envelope entry. A deadline that cannot be represented as a `SystemTime` has
/// no entry: stamping a past value would read as an exhausted budget downstream.
pub(super) fn activity_deadline_env(wall_clock_timeout: Duration) -> Option<(String, String)> {
    let since_epoch = SystemTime::now()
        .checked_add(wall_clock_timeout)?
        .duration_since(UNIX_EPOCH)
        .ok()?;
    let deadline_ms = u64::try_from(since_epoch.as_millis()).unwrap_or(u64::MAX);
    Some((ACTIVITY_DEADLINE_ENV.to_string(), deadline_ms.to_string()))
}

/// The `CODEX_HOME` a Codex child resolves from its environment: the
/// `[execution.env]` value, else `$HOME/.codex`. [ORB-14696]
pub(super) fn codex_home(env: &[(String, String)]) -> Option<PathBuf> {
    let value = |name: &str| {
        env.iter()
            .rev()
            .find(|(key, value)| key == name && !value.is_empty())
            .map(|(_, value)| value.as_str())
    };
    value("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| value("HOME").map(|home| Path::new(home).join(".codex")))
}

/// Refuse to start `claude` on a host where the Claude Desktop app's shared
/// login would silently stand in for a missing worker credential [ORB-15154].
///
/// The Desktop revokes that login when it refreshes, so the run would fail
/// with a 401 part-way through. Failing here names the variable and where to
/// set it instead. Only the names are inspected, never the values.
pub(super) fn require_claude_worker_credential(
    host: &dyn RuntimeHost,
    provider: &str,
    child_env: &[(String, String)],
) -> Result<(), DispatchError> {
    if provider != "claude" || !host.requires_claude_worker_credential() {
        return Ok(());
    }
    let held = CLAUDE_WORKER_CREDENTIAL_ENV.iter().any(|name| {
        child_env
            .iter()
            .any(|(key, value)| key == name && !value.is_empty())
    });
    if held {
        return Ok(());
    }
    Err(DispatchError::CliInvocationPermanent(
        "claude activity refused: neither CLAUDE_CODE_OAUTH_TOKEN nor ANTHROPIC_API_KEY \
         reached the provider environment, so `claude` would fall back to the Claude \
         Desktop login, which the Desktop revokes mid-run (HTTP 401). Create a worker \
         token with `claude setup-token`, export CLAUDE_CODE_OAUTH_TOKEN in ~/.zprofile \
         and list it in `[execution.env] pass` (global or workspace config.toml). For \
         runs started by the clock, also put it in ~/.orbit/clock.env (mode 600). \
         `orbit doctor` checks each of these."
            .to_string(),
    ))
}

/// Compose the provider environment while admitting Codex's two documented
/// CA-bundle overrides only when Orbit's macOS wrapper needs them.
///
/// The variables remain outside the general agent baseline: other providers,
/// bare Codex invocations, and Linux keep their existing environment surface.
/// The macOS spawn layer supplies a public system bundle only when neither
/// explicit value is present.
pub(crate) fn provider_child_environment(
    host: &dyn RuntimeHost,
    provider: &str,
    sandbox: Option<&super::super::super::dispatcher::ResolvedSandbox>,
    required_env_vars: &[&str],
) -> Vec<(String, String)> {
    let needs_codex_ca_overrides = provider == "codex"
        && sandbox.is_some_and(|sandbox| sandbox.kind == ExecutorSandboxKind::MacosSandboxExec);
    if !needs_codex_ca_overrides {
        return host.agent_subprocess_environment(required_env_vars);
    }

    let mut env_names = required_env_vars.to_vec();
    env_names.extend([CODEX_CA_CERTIFICATE_ENV, SSL_CERT_FILE_ENV]);
    host.agent_subprocess_environment(&env_names)
}
