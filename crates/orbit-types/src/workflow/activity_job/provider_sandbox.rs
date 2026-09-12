//! Inner sandbox modes a provider CLI accepts.
//!
//! Distinct from Orbit's OS executor sandbox (`ExecutorSandboxKind`). Codex is
//! the only shipped provider whose inner sandbox is dynamically configured
//! (`codex exec --sandbox`); other providers have no named inner-sandbox flag
//! Orbit can tighten per invocation, so their mode is `default`.

use super::Provider;

/// Mode used when a provider has no configurable inner sandbox.
pub const DEFAULT_PROVIDER_SANDBOX: &str = "default";

/// Codex `--sandbox` values admitted by `[execution.codex].sandbox`.
pub const CODEX_PROVIDER_SANDBOX_MODES: &[&str] =
    &["read-only", "workspace-write", "danger-full-access"];

/// Codex's least-restrictive inner sandbox. Enables host integrations such as
/// Computer Use and the browser, which is why an operator invocation must
/// name it.
pub const CODEX_LEAST_RESTRICTIVE_SANDBOX: &str = "danger-full-access";

/// Inner-sandbox modes the named provider accepts on `orbit.agent.invoke`.
pub fn provider_sandbox_modes(provider: Provider) -> &'static [&'static str] {
    match provider {
        Provider::Codex => CODEX_PROVIDER_SANDBOX_MODES,
        _ => &[DEFAULT_PROVIDER_SANDBOX],
    }
}

/// The provider's least-restrictive inner sandbox, when it has a named one.
///
/// Codex is `danger-full-access`. Providers without a configurable inner
/// sandbox return `None`: `default` is not a sandbox mode that can be
/// tightened, so it does not trigger the host-integration warning.
pub fn least_restrictive_provider_sandbox(provider: Provider) -> Option<&'static str> {
    match provider {
        Provider::Codex => Some(CODEX_LEAST_RESTRICTIVE_SANDBOX),
        _ => None,
    }
}

/// Format the operator-facing label stored on the run and returned by
/// `orbit.agent.invoke` / `orbit run show`.
pub fn format_provider_sandbox(provider: Provider, mode: &str) -> String {
    format!("{}:{mode}", provider.as_str())
}

/// Split a persisted `provider:mode` label.
pub fn parse_provider_sandbox_label(label: &str) -> Option<(Provider, &str)> {
    let (provider, mode) = label.split_once(':')?;
    let provider = Provider::parse(provider).ok()?;
    if mode.is_empty() {
        return None;
    }
    Some((provider, mode))
}

/// Admit an override mode for this provider, or name the supported values.
pub fn admit_provider_sandbox_mode(provider: Provider, mode: &str) -> Result<&str, String> {
    let trimmed = mode.trim();
    if trimmed.is_empty() {
        return Err(format!(
            "`provider_sandbox` is empty; expected one of {}",
            provider_sandbox_modes(provider).join(", ")
        ));
    }
    if provider_sandbox_modes(provider).contains(&trimmed) {
        return Ok(trimmed);
    }
    Err(format!(
        "`provider_sandbox` `{trimmed}` is not supported for {}; expected one of {}",
        provider.as_str(),
        provider_sandbox_modes(provider).join(", ")
    ))
}

/// Whether `mode` is this provider's least-restrictive inner sandbox.
pub fn is_least_restrictive_provider_sandbox(provider: Provider, mode: &str) -> bool {
    least_restrictive_provider_sandbox(provider) == Some(mode)
}

/// Operator-facing warning when the provider inner sandbox is least-restrictive.
pub fn least_restrictive_provider_sandbox_warning(label: &str) -> String {
    format!(
        "provider runs with {label}; it may use host integrations (browser, computer use, …) \
         beyond the working directory"
    )
}
