//! Provider credentials, deadlines and child environment composition.
use super::super::super::dispatcher::DispatchError;
use super::super::spawn::{CODEX_CA_CERTIFICATE_ENV, SSL_CERT_FILE_ENV};
use crate::context::RuntimeHost;
use orbit_common::security::child_env::{ACTIVITY_DEADLINE_ENV, CLAUDE_CREDENTIAL_ENV};
use orbit_types::identity::CrewEnvVar;
use orbit_types::workflow::ExecutorSandboxKind;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// How to supply a worker credential, the remediation a refused Claude activity
/// and a drain's submission both name [ORB-16663].
pub const CLAUDE_WORKER_CREDENTIAL_REMEDY: &str = "Create a worker token with `claude \
     setup-token`, export CLAUDE_CODE_OAUTH_TOKEN in ~/.zprofile and list it in \
     `[execution.env] pass` (global or workspace config.toml). For runs started by the \
     clock, also put it in ~/.orbit/clock.env (mode 600). `orbit doctor` checks each of \
     these.";

/// Whether a child environment holds one of the credentials that keep `claude`
/// off the Desktop login. Only the names are inspected, never the values.
pub fn claude_worker_credential_held(child_env: &[(String, String)]) -> bool {
    CLAUDE_CREDENTIAL_ENV.iter().any(|name| {
        child_env
            .iter()
            .any(|(key, value)| key == name && !value.is_empty())
    })
}

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
    if provider != "claude"
        || !host.requires_claude_worker_credential()
        || claude_worker_credential_held(child_env)
    {
        return Ok(());
    }
    Err(DispatchError::CliInvocationPermanent(format!(
        "claude activity refused: neither CLAUDE_CODE_OAUTH_TOKEN nor ANTHROPIC_API_KEY \
         reached the provider environment, so `claude` would fall back to the Claude \
         Desktop login, which the Desktop revokes mid-run (HTTP 401). \
         {CLAUDE_WORKER_CREDENTIAL_REMEDY}"
    )))
}

/// Refuse to start an agent whose crew sets an `[crews.<name>] env` variable
/// the composed environment does not hold: its source was unset where the
/// host launched. Starting anyway would run the crew on whatever login the
/// provider falls back to, which is exactly what the crew's entry exists to
/// avoid. Names only, never values.
pub(super) fn require_crew_env(
    crew_env: &[CrewEnvVar],
    child_env: &[(String, String)],
) -> Result<(), DispatchError> {
    let missing: Vec<String> = crew_env
        .iter()
        .filter(|entry| {
            !child_env
                .iter()
                .any(|(key, value)| key == &entry.name && !value.is_empty())
        })
        .map(|entry| {
            if entry.is_mapped() {
                format!("{} (from {})", entry.name, entry.source)
            } else {
                entry.name.clone()
            }
        })
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    Err(DispatchError::CliInvocationPermanent(format!(
        "activity refused: its crew's `env` sets {}, but the source is unset or empty where \
         this run was launched, so the provider would fall back to another login. Export the \
         source before starting the run (for clock-started runs, put it in \
         ~/.orbit/clock.env), or select another crew",
        missing.join(", ")
    )))
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
    crew_env: &[CrewEnvVar],
    required_env_vars: &[&str],
) -> Vec<(String, String)> {
    let needs_codex_ca_overrides = provider == "codex"
        && sandbox.is_some_and(|sandbox| sandbox.kind == ExecutorSandboxKind::MacosSandboxExec);
    let mut env_names = required_env_vars.to_vec();
    if needs_codex_ca_overrides {
        env_names.extend([CODEX_CA_CERTIFICATE_ENV, SSL_CERT_FILE_ENV]);
    }
    if crew_env.is_empty() {
        host.agent_subprocess_environment(&env_names)
    } else {
        host.crew_agent_subprocess_environment(crew_env, &env_names)
    }
}

/// Hand the agent the host's headless browser kit when it is verified
/// (`orbit browser-kit verify`), so a UI check needs no host-specific path.
///
/// Orbit adds the three variables itself rather than admitting them from the
/// parent, so the allowlist stays explicit, and replaces any same-named value
/// the operator's policy forwarded. Without a verified kit nothing is added.
/// The kit is the one under the agent's own `HOME`. Its unpacked Linux
/// libraries never go on this environment's `LD_LIBRARY_PATH`, where they
/// could shadow system libraries for cargo and tests: only the shell that
/// sources `$ORBIT_BROWSER_KIT_ENV` gets them.
pub(super) fn add_browser_kit_env(child_env: &mut Vec<(String, String)>) {
    let Some(home) = child_env
        .iter()
        .rev()
        .find(|(key, value)| key == "HOME" && !value.is_empty())
        .map(|(_, home)| PathBuf::from(home))
    else {
        return;
    };
    let kit_env = orbit_exec::browser_kit::worker_env_for_home(&home);
    if kit_env.is_empty() {
        return;
    }
    child_env.retain(|(key, _)| !orbit_exec::browser_kit::WORKER_ENV_NAMES.contains(&key.as_str()));
    child_env.extend(kit_env);
}
