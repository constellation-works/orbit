//! Unified secret redaction.
//!
//! Consolidates the three surfaces scattered across the workspace today:
//! - env-value scrubbing previously reached through the old shared types
//!   re-exports
//! - [`PatternRedactor`] — regex-based patterns for `Authorization` / `x-api-key` / `Bearer` in
//!   HTTP-shaped payloads (headers, JSON)
//! - `orbit_engine::activity_job::cli_runner::ArgvRedactor` — the above plus a raw
//!   `sk-…` pattern for argv that leaks provider keys
//!
//! This module is the single source of truth for generic, domain-free
//! redaction, including the `OrbitError`-aware helper now that both the
//! utilities and domain types live in the same crate.
//!
//! Callers pick the layer they need:
//! - [`redact_sensitive_env_text`] — scrub live env-var values from a string,
//!   including the JSON-string and Rust `Debug` encodings of those values
//! - [`PatternRedactor`] — regex pattern scrubbing (HTTP / argv / JSON / SSH diagnostics)
//! - [`redact_all`] — env + default patterns in one pass (use when you don't
//!   know what shape the input has and want maximum coverage)

mod env;
mod error;
mod pattern;

pub use env::{
    is_sensitive_env_name, redact_sensitive_env_bytes, redact_sensitive_env_json,
    redact_sensitive_env_text,
};
pub use error::{
    credential_safe_location, redact_all_and_home, redact_all_and_home_error, redact_all_error,
    redact_all_json, redact_home_dir,
};
pub use pattern::{
    PatternRedactor, argv_redactor, is_high_confidence_single_token_credential, redact_all,
};
