//! Inner sandbox modes accepted by the Codex provider configuration.

/// Codex `--sandbox` values admitted by `[execution.codex].sandbox`.
pub const CODEX_PROVIDER_SANDBOX_MODES: &[&str] =
    &["read-only", "workspace-write", "danger-full-access"];
