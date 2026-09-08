//! Unified secret redaction.
//!
//! Consolidates the three surfaces scattered across the workspace today:
//! - env-value scrubbing previously reached through the old shared types
//!   re-exports
//! - `orbit_agent::loop_engine::audit::redaction::RedactionMiddleware` —
//!   regex-based patterns for `Authorization` / `x-api-key` / `Bearer` in
//!   HTTP-shaped payloads (headers, JSON)
//! - `orbit_engine::activity_job::cli_runner::ArgvRedactor` — the above plus a raw
//!   `sk-…` pattern for argv that leaks provider keys
//!
//! This module is the single source of truth for generic, domain-free
//! redaction, including the `OrbitError`-aware helper now that both the
//! utilities and domain types live in the same crate.
//!
//! Callers pick the layer they need:
//! - [`redact_sensitive_env_text`] — scrub live env-var values from a string
//! - [`PatternRedactor`] — regex pattern scrubbing (HTTP / argv / JSON / SSH diagnostics)
//! - [`redact_all`] — env + default patterns in one pass (use when you don't
//!   know what shape the input has and want maximum coverage)

// Existing expect calls in this module document local invariants; keep the allow scoped while the workspace lint is ratcheted.
#![allow(clippy::expect_used)]

use std::{
    borrow::Cow,
    path::{Component, Path},
    sync::OnceLock,
};

use regex::Regex;
use serde_json::Value;

use crate::{
    ArtifactOrigin, DependencyNotDelivered, FrictionNotLocal, OrbitError, RecoverableVcsConflict,
    WorkspaceClaimHeld,
};

const REDACTED_ENV_VALUE: &str = "[REDACTED_ENV]";
static DEFAULT_PATTERN_REDACTOR: OnceLock<PatternRedactor> = OnceLock::new();
static HIGH_CONFIDENCE_SINGLE_TOKEN_PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();

// ---------------------------------------------------------------------------
// Env-var value scrubbing
// ---------------------------------------------------------------------------

/// Replace occurrences of any sensitive env-var value (as seen in the live
/// process environment) with `[REDACTED_ENV]`.
///
/// "Sensitive" is matched against the var *name* — anything containing
/// SECRET / TOKEN / PASSWORD / API_KEY / etc. See [`is_sensitive_env_name`].
/// Only values that pass `is_redactable_value` are substituted, so an
/// ordinary word held by a sensitive-named variable is left untouched.
pub fn redact_sensitive_env_text(raw: &str) -> String {
    let mut redacted = raw.to_string();
    for secret in sensitive_env_values() {
        redacted = redacted.replace(&secret, REDACTED_ENV_VALUE);
    }
    redacted
}

pub fn redact_sensitive_env_option(raw: Option<String>) -> Option<String> {
    raw.map(|value| redact_sensitive_env_text(&value))
}

pub fn redact_sensitive_env_json(value: Value) -> Value {
    match value {
        Value::String(raw) => Value::String(redact_sensitive_env_text(&raw)),
        Value::Array(items) => {
            Value::Array(items.into_iter().map(redact_sensitive_env_json).collect())
        }
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(key, value)| (key, redact_sensitive_env_json(value)))
                .collect(),
        ),
        other => other,
    }
}

/// Replace `$HOME` / `$USERPROFILE` with `~` in the given string. Prevents
/// user-identifiable paths from leaking into logs. Addresses CodeQL
/// `rust/cleartext-logging`.
pub fn redact_home_dir(text: &str) -> String {
    if let Some(home) = home_dir_string() {
        redact_path_prefix(text, &home)
    } else {
        text.to_string()
    }
}

fn redact_path_prefix(text: &str, prefix: &str) -> String {
    let mut redacted = String::with_capacity(text.len());
    let mut remaining = text;

    while let Some(index) = remaining.find(prefix) {
        let (before_match, match_and_after) = remaining.split_at(index);
        let after_match = &match_and_after[prefix.len()..];

        redacted.push_str(before_match);
        if after_match.is_empty() || after_match.starts_with('/') {
            redacted.push('~');
        } else {
            redacted.push_str(prefix);
        }
        remaining = after_match;
    }

    redacted.push_str(remaining);
    redacted
}

/// Apply env-value redaction to the message carried by any `OrbitError` variant.
pub fn redact_sensitive_env_error(error: OrbitError) -> OrbitError {
    match error {
        OrbitError::PolicyDenied(m) => OrbitError::PolicyDenied(redact_sensitive_env_text(&m)),
        OrbitError::NotFound { kind, id } => OrbitError::NotFound {
            kind,
            id: redact_sensitive_env_text(&id),
        },
        OrbitError::CapabilityDenied(m) => {
            OrbitError::CapabilityDenied(redact_sensitive_env_text(&m))
        }
        OrbitError::UnknownSelector(m) => {
            OrbitError::UnknownSelector(redact_sensitive_env_text(&m))
        }
        OrbitError::AmbiguousCaller(m) => {
            OrbitError::AmbiguousCaller(redact_sensitive_env_text(&m))
        }
        OrbitError::UnauthorizedCaller(m) => {
            OrbitError::UnauthorizedCaller(redact_sensitive_env_text(&m))
        }
        OrbitError::AmbiguousDestination(m) => {
            OrbitError::AmbiguousDestination(redact_sensitive_env_text(&m))
        }
        OrbitError::UnreachableDestination(m) => {
            OrbitError::UnreachableDestination(redact_sensitive_env_text(&m))
        }
        OrbitError::StaleRoute(m) => OrbitError::StaleRoute(redact_sensitive_env_text(&m)),
        OrbitError::UnhealthyCheckout(m) => {
            OrbitError::UnhealthyCheckout(redact_sensitive_env_text(&m))
        }
        OrbitError::ToolNotOnThisHost(m) => {
            OrbitError::ToolNotOnThisHost(redact_sensitive_env_text(&m))
        }
        OrbitError::CapabilityRefused(m) => {
            OrbitError::CapabilityRefused(redact_sensitive_env_text(&m))
        }
        OrbitError::AdrInvalidTransition(m) => {
            OrbitError::AdrInvalidTransition(redact_sensitive_env_text(&m))
        }
        OrbitError::RemoteArtifactUnavailable {
            kind,
            id,
            artifact_origin,
        } => OrbitError::RemoteArtifactUnavailable {
            kind,
            id: redact_sensitive_env_text(&id),
            artifact_origin: redact_artifact_origin(artifact_origin, redact_sensitive_env_text),
        },
        OrbitError::ArtifactNotLocal {
            kind,
            id,
            artifact_origin,
        } => OrbitError::ArtifactNotLocal {
            kind,
            id: redact_sensitive_env_text(&id),
            artifact_origin: redact_artifact_origin(artifact_origin, redact_sensitive_env_text),
        },
        OrbitError::FrictionNotLocal(details) => OrbitError::FrictionNotLocal(
            redact_friction_not_local(*details, redact_sensitive_env_text),
        ),
        OrbitError::CompanionNotInstalled(m) => {
            OrbitError::CompanionNotInstalled(redact_sensitive_env_text(&m))
        }
        OrbitError::InvalidInput(m) => OrbitError::InvalidInput(redact_sensitive_env_text(&m)),
        OrbitError::SensitiveInput { field, reason } => OrbitError::SensitiveInput {
            field: redact_sensitive_env_text(&field),
            reason: redact_sensitive_env_text(&reason),
        },
        OrbitError::InvalidInputDiagnostic {
            message,
            did_you_mean,
        } => OrbitError::InvalidInputDiagnostic {
            message: redact_sensitive_env_text(&message),
            did_you_mean: did_you_mean
                .into_iter()
                .map(|suggestion| redact_sensitive_env_text(&suggestion))
                .collect(),
        },
        OrbitError::SkillValidation(m) => {
            OrbitError::SkillValidation(redact_sensitive_env_text(&m))
        }
        OrbitError::JobValidation(m) => OrbitError::JobValidation(redact_sensitive_env_text(&m)),
        OrbitError::AgentProtocolViolation(m) => {
            OrbitError::AgentProtocolViolation(redact_sensitive_env_text(&m))
        }
        OrbitError::UnsupportedAgentProvider(m) => {
            OrbitError::UnsupportedAgentProvider(redact_sensitive_env_text(&m))
        }
        OrbitError::OwnerUnavailable(m) => {
            OrbitError::OwnerUnavailable(redact_sensitive_env_text(&m))
        }
        OrbitError::OwnerNegotiation(m) => {
            OrbitError::OwnerNegotiation(redact_sensitive_env_text(&m))
        }
        OrbitError::OutcomeUnknown {
            mcp_call_id,
            message,
        } => OrbitError::OutcomeUnknown {
            mcp_call_id: redact_sensitive_env_text(&mcp_call_id),
            message: redact_sensitive_env_text(&message),
        },
        OrbitError::RemoteTool {
            code,
            message,
            payload,
        } => OrbitError::RemoteTool {
            code: redact_sensitive_env_text(&code),
            message: redact_sensitive_env_text(&message),
            payload: redact_sensitive_env_json(payload),
        },
        OrbitError::Execution(m) => OrbitError::Execution(redact_sensitive_env_text(&m)),
        OrbitError::RecoverableVcsConflict(conflict) => OrbitError::RecoverableVcsConflict(
            redact_recoverable_vcs_conflict(*conflict, redact_sensitive_env_text),
        ),
        OrbitError::RunCancellationIncomplete {
            pid,
            pgid,
            term_sent,
            kill_sent,
            leader_alive,
            group_alive,
        } => OrbitError::RunCancellationIncomplete {
            pid,
            pgid,
            term_sent,
            kill_sent,
            leader_alive,
            group_alive,
        },
        OrbitError::TaskBundleCorrupt {
            task_id,
            path,
            reason,
        } => OrbitError::TaskBundleCorrupt {
            task_id: redact_sensitive_env_text(&task_id),
            path: redact_sensitive_env_text(&path),
            reason: redact_sensitive_env_text(&reason),
        },
        OrbitError::FileLockTimeout(timeout) => OrbitError::FileLockTimeout(
            redact_file_lock_timeout(*timeout, redact_sensitive_env_text),
        ),
        OrbitError::Store(m) => OrbitError::Store(redact_sensitive_env_text(&m)),
        OrbitError::TaskStatusTransition(m) => {
            OrbitError::TaskStatusTransition(redact_sensitive_env_text(&m))
        }
        OrbitError::DependencyNotDelivered(diagnostic) => OrbitError::DependencyNotDelivered(
            redact_dependency_not_delivered(*diagnostic, redact_sensitive_env_text),
        ),
        OrbitError::ShipRunInFlight { task_id, run_id } => OrbitError::ShipRunInFlight {
            task_id: redact_sensitive_env_text(&task_id),
            run_id: redact_sensitive_env_text(&run_id),
        },
        OrbitError::WorkspaceClaimHeld(claim) => OrbitError::WorkspaceClaimHeld(
            redact_workspace_claim_held(*claim, redact_sensitive_env_text),
        ),
        OrbitError::JobRunStateTransition(m) => {
            OrbitError::JobRunStateTransition(redact_sensitive_env_text(&m))
        }
        OrbitError::JobRunStartConflict(m) => {
            OrbitError::JobRunStartConflict(redact_sensitive_env_text(&m))
        }
        OrbitError::JobRunControlConflict(m) => {
            OrbitError::JobRunControlConflict(redact_sensitive_env_text(&m))
        }
        OrbitError::Io(m) => OrbitError::Io(redact_sensitive_env_text(&m)),
        OrbitError::WorkspaceError(m) => OrbitError::WorkspaceError(redact_sensitive_env_text(&m)),
        OrbitError::Migration(m) => OrbitError::Migration(redact_sensitive_env_text(&m)),
    }
}

/// Scrub an [`OrbitError`]'s string payloads with the full [`redact_all`]
/// pipeline (live env values **plus** the HTTP header / bearer / provider-key
/// patterns), not just env values like [`redact_sensitive_env_error`].
/// [ORB-00417] Apply at the error persistence/log boundary so an error message
/// embedding a `Bearer <token>` or `sk-*` key in a URL is never written out
/// un-redacted. Idempotent: `redact_all` placeholders never re-match the secret
/// patterns.
pub fn redact_all_error(error: OrbitError) -> OrbitError {
    match error {
        OrbitError::PolicyDenied(m) => OrbitError::PolicyDenied(redact_all(&m)),
        OrbitError::NotFound { kind, id } => OrbitError::NotFound {
            kind,
            id: redact_all(&id),
        },
        OrbitError::CapabilityDenied(m) => OrbitError::CapabilityDenied(redact_all(&m)),
        OrbitError::UnknownSelector(m) => OrbitError::UnknownSelector(redact_all(&m)),
        OrbitError::AmbiguousCaller(m) => OrbitError::AmbiguousCaller(redact_all(&m)),
        OrbitError::UnauthorizedCaller(m) => OrbitError::UnauthorizedCaller(redact_all(&m)),
        OrbitError::AmbiguousDestination(m) => OrbitError::AmbiguousDestination(redact_all(&m)),
        OrbitError::UnreachableDestination(m) => OrbitError::UnreachableDestination(redact_all(&m)),
        OrbitError::StaleRoute(m) => OrbitError::StaleRoute(redact_all(&m)),
        OrbitError::UnhealthyCheckout(m) => OrbitError::UnhealthyCheckout(redact_all(&m)),
        OrbitError::ToolNotOnThisHost(m) => OrbitError::ToolNotOnThisHost(redact_all(&m)),
        OrbitError::CapabilityRefused(m) => OrbitError::CapabilityRefused(redact_all(&m)),
        OrbitError::AdrInvalidTransition(m) => OrbitError::AdrInvalidTransition(redact_all(&m)),
        OrbitError::RemoteArtifactUnavailable {
            kind,
            id,
            artifact_origin,
        } => OrbitError::RemoteArtifactUnavailable {
            kind,
            id: redact_all(&id),
            artifact_origin: redact_artifact_origin(artifact_origin, redact_all),
        },
        OrbitError::ArtifactNotLocal {
            kind,
            id,
            artifact_origin,
        } => OrbitError::ArtifactNotLocal {
            kind,
            id: redact_all(&id),
            artifact_origin: redact_artifact_origin(artifact_origin, redact_all),
        },
        OrbitError::FrictionNotLocal(details) => {
            OrbitError::FrictionNotLocal(redact_friction_not_local(*details, redact_all))
        }
        OrbitError::CompanionNotInstalled(m) => OrbitError::CompanionNotInstalled(redact_all(&m)),
        OrbitError::InvalidInput(m) => OrbitError::InvalidInput(redact_all(&m)),
        OrbitError::SensitiveInput { field, reason } => OrbitError::SensitiveInput {
            field: redact_all(&field),
            reason: redact_all(&reason),
        },
        OrbitError::InvalidInputDiagnostic {
            message,
            did_you_mean,
        } => OrbitError::InvalidInputDiagnostic {
            message: redact_all(&message),
            did_you_mean: did_you_mean
                .into_iter()
                .map(|suggestion| redact_all(&suggestion))
                .collect(),
        },
        OrbitError::SkillValidation(m) => OrbitError::SkillValidation(redact_all(&m)),
        OrbitError::JobValidation(m) => OrbitError::JobValidation(redact_all(&m)),
        OrbitError::AgentProtocolViolation(m) => OrbitError::AgentProtocolViolation(redact_all(&m)),
        OrbitError::UnsupportedAgentProvider(m) => {
            OrbitError::UnsupportedAgentProvider(redact_all(&m))
        }
        OrbitError::OwnerUnavailable(m) => OrbitError::OwnerUnavailable(redact_all(&m)),
        OrbitError::OwnerNegotiation(m) => OrbitError::OwnerNegotiation(redact_all(&m)),
        OrbitError::OutcomeUnknown {
            mcp_call_id,
            message,
        } => OrbitError::OutcomeUnknown {
            mcp_call_id: redact_all(&mcp_call_id),
            message: redact_all(&message),
        },
        OrbitError::RemoteTool {
            code,
            message,
            payload,
        } => OrbitError::RemoteTool {
            code: redact_all(&code),
            message: redact_all(&message),
            payload: redact_json_with(payload, redact_all),
        },
        OrbitError::Execution(m) => OrbitError::Execution(redact_all(&m)),
        OrbitError::RecoverableVcsConflict(conflict) => OrbitError::RecoverableVcsConflict(
            redact_recoverable_vcs_conflict(*conflict, redact_all),
        ),
        OrbitError::RunCancellationIncomplete {
            pid,
            pgid,
            term_sent,
            kill_sent,
            leader_alive,
            group_alive,
        } => OrbitError::RunCancellationIncomplete {
            pid,
            pgid,
            term_sent,
            kill_sent,
            leader_alive,
            group_alive,
        },
        OrbitError::TaskBundleCorrupt {
            task_id,
            path,
            reason,
        } => OrbitError::TaskBundleCorrupt {
            task_id: redact_all(&task_id),
            path: redact_all(&path),
            reason: redact_all(&reason),
        },
        OrbitError::FileLockTimeout(timeout) => {
            OrbitError::FileLockTimeout(redact_file_lock_timeout(*timeout, redact_all))
        }
        OrbitError::Store(m) => OrbitError::Store(redact_all(&m)),
        OrbitError::TaskStatusTransition(m) => OrbitError::TaskStatusTransition(redact_all(&m)),
        OrbitError::DependencyNotDelivered(diagnostic) => OrbitError::DependencyNotDelivered(
            redact_dependency_not_delivered(*diagnostic, redact_all),
        ),
        OrbitError::ShipRunInFlight { task_id, run_id } => OrbitError::ShipRunInFlight {
            task_id: redact_all(&task_id),
            run_id: redact_all(&run_id),
        },
        OrbitError::WorkspaceClaimHeld(claim) => {
            OrbitError::WorkspaceClaimHeld(redact_workspace_claim_held(*claim, redact_all))
        }
        OrbitError::JobRunStateTransition(m) => OrbitError::JobRunStateTransition(redact_all(&m)),
        OrbitError::JobRunStartConflict(m) => OrbitError::JobRunStartConflict(redact_all(&m)),
        OrbitError::JobRunControlConflict(m) => OrbitError::JobRunControlConflict(redact_all(&m)),
        OrbitError::Io(m) => OrbitError::Io(redact_all(&m)),
        OrbitError::WorkspaceError(m) => OrbitError::WorkspaceError(redact_all(&m)),
        OrbitError::Migration(m) => OrbitError::Migration(redact_all(&m)),
    }
}

fn redact_file_lock_timeout(
    mut timeout: crate::fs::io::FileLockTimeout,
    redact: fn(&str) -> String,
) -> Box<crate::fs::io::FileLockTimeout> {
    timeout.lock_path = redact(&timeout.lock_path.to_string_lossy()).into();
    timeout.label = redact(&timeout.label);
    if let Some(holder) = &mut timeout.holder {
        holder.acquired_at = redact(&holder.acquired_at);
        holder.label = redact(&holder.label);
    }
    Box::new(timeout)
}

fn redact_friction_not_local(
    details: FrictionNotLocal,
    redact: fn(&str) -> String,
) -> Box<FrictionNotLocal> {
    Box::new(FrictionNotLocal {
        friction_id: redact(&details.friction_id),
        task_id: redact(&details.task_id),
        workspace_id: redact(&details.workspace_id),
        found_in: details
            .found_in
            .into_iter()
            .map(|workspace_id| redact(&workspace_id))
            .collect(),
    })
}

fn redact_workspace_claim_held(
    claim: WorkspaceClaimHeld,
    redact: fn(&str) -> String,
) -> Box<WorkspaceClaimHeld> {
    Box::new(WorkspaceClaimHeld {
        operation: redact(&claim.operation),
        holder: redact(&claim.holder),
        claim_id: redact(&claim.claim_id),
        expires_at: redact(&claim.expires_at),
    })
}

fn redact_dependency_not_delivered(
    diagnostic: DependencyNotDelivered,
    redact: fn(&str) -> String,
) -> Box<DependencyNotDelivered> {
    Box::new(DependencyNotDelivered {
        task_id: redact(&diagnostic.task_id),
        dependency_id: redact(&diagnostic.dependency_id),
        base_ref: redact(&diagnostic.base_ref),
        base_sha: redact(&diagnostic.base_sha),
        detail: redact(&diagnostic.detail),
    })
}

fn redact_recoverable_vcs_conflict(
    conflict: RecoverableVcsConflict,
    redact: fn(&str) -> String,
) -> Box<RecoverableVcsConflict> {
    Box::new(RecoverableVcsConflict {
        operation: redact(&conflict.operation),
        original_base_sha: redact(&conflict.original_base_sha),
        target_base_sha: redact(&conflict.target_base_sha),
        conflicting_paths: conflict
            .conflicting_paths
            .into_iter()
            .map(|path| redact(&path))
            .collect(),
        diagnostic: redact(&conflict.diagnostic),
    })
}

fn redact_json_with(value: Value, redact: fn(&str) -> String) -> Value {
    match value {
        Value::String(raw) => Value::String(redact(&raw)),
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(|item| redact_json_with(item, redact))
                .collect(),
        ),
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(key, value)| (key, redact_json_with(value, redact)))
                .collect(),
        ),
        other => other,
    }
}

fn redact_artifact_origin(
    artifact_origin: ArtifactOrigin,
    redact: fn(&str) -> String,
) -> ArtifactOrigin {
    ArtifactOrigin {
        mode: artifact_origin.mode,
        worktree_root: credential_safe_location_with(&artifact_origin.worktree_root, redact),
        branch: artifact_origin
            .branch
            .map(|branch| credential_safe_location_with(&branch, redact)),
    }
}

/// Return a public-safe worktree or branch location. URI-shaped values are
/// rejected wholesale so allocation metadata can never expose a credentialed
/// transport target; ordinary filesystem paths retain their useful location
/// while the standard credential patterns are scrubbed.
pub fn credential_safe_location(raw: &str) -> String {
    credential_safe_location_with(raw, redact_all)
}

fn credential_safe_location_with(raw: &str, redact: fn(&str) -> String) -> String {
    let lower = raw.trim().to_ascii_lowercase();
    if lower.contains("://") || lower.starts_with("bearer ") || lower.contains("authorization:") {
        "[REDACTED_LOCATION]".to_string()
    } else {
        redact(raw)
    }
}

fn home_dir_string() -> Option<String> {
    std::env::var("HOME")
        .ok()
        .or_else(|| std::env::var("USERPROFILE").ok())
        .filter(|home| {
            let path = Path::new(home);
            path.is_absolute()
                && path
                    .components()
                    .any(|component| matches!(component, Component::Normal(_)))
        })
}

fn sensitive_env_values() -> Vec<String> {
    let mut values = std::env::vars()
        .filter(|(name, value)| is_sensitive_env_name(name) && is_redactable_value(value))
        .map(|(_, value)| value)
        .collect::<Vec<_>>();
    values.sort_by_key(|value| std::cmp::Reverse(value.len()));
    values.dedup();
    values
}

/// Decide whether a sensitive-named env value is eligible for substitution
/// by [`redact_sensitive_env_text`].
///
/// A value is eligible only when, after trim, it is at least 4 characters
/// **and** it contains at least one non-letter (digit or punctuation).
/// All-letter values are treated as ordinary words (`user`, `true`, `none`,
/// `root`, `main`, `test`, `prod`, `local`, `auto`) and are never
/// substituted, even when a sensitive-looking variable name happens to
/// hold them. This is a shape gate, not a raised length floor: a short
/// secret such as `a1b2` remains eligible.
///
/// Eligible values are still matched with a bare substring replace so
/// embedded tokens in URLs and concatenated log fragments stay scrubbed.
///
/// False-negative accepted: an all-letter secret or passphrase with no
/// digits or punctuation (a dictionary word, or concatenated words with
/// no separators) is not env-substituted. Pattern-based redaction still
/// independently catches provider-shaped tokens (`ghp_…`, `sk-…`).
// pub(crate) for sibling-layout tests in utility/tests/redaction.rs.
pub(crate) fn is_redactable_value(value: &str) -> bool {
    let trimmed = value.trim();
    trimmed.len() >= 4 && trimmed.chars().any(|c| !c.is_alphabetic())
}

pub fn is_sensitive_env_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    upper.contains("SECRET")
        || upper.contains("TOKEN")
        || upper.contains("PASSWORD")
        || upper.contains("PASSWD")
        || upper.contains("PASSCODE")
        || upper.contains("API_KEY")
        || upper.ends_with("_KEY")
        || upper.contains("PRIVATE")
        || upper.contains("CREDENTIAL")
        || upper.contains("COOKIE")
        || upper.contains("SESSION")
        || upper.contains("BEARER")
        || upper.contains("AUTH")
}

// ---------------------------------------------------------------------------
// Pattern-based redaction (HTTP + argv)
// ---------------------------------------------------------------------------

/// Regex-driven scrubber for credential shapes and infrastructure identifiers.
///
/// Builds to `default()` cover Authorization / x-api-key / URL key params /
/// Bearer / raw header lines, high-confidence provider credentials, and
/// structurally recognizable SSH fingerprints, public-key comments, and
/// connection hosts. Use [`PatternRedactor::with_argv_secrets`] to also catch
/// short bare `sk-…` tokens — needed when scrubbing subprocess argv where a
/// provider key sometimes ends up mis-configured.
pub struct PatternRedactor {
    patterns: Vec<(Regex, &'static str)>,
}

impl PatternRedactor {
    /// Shared default for unknown-shape persisted text.
    pub fn http_default() -> Self {
        let patterns = vec![
            (
                Regex::new(r#"(?i)"authorization"\s*:\s*"[^"]*""#).expect("valid regex"),
                r#""authorization":"[REDACTED_AUTH]""#,
            ),
            (
                Regex::new(r#"(?i)"x-api-key"\s*:\s*"[^"]*""#).expect("valid regex"),
                r#""x-api-key":"[REDACTED_AUTH]""#,
            ),
            (
                Regex::new(r#"(?i)"api[_-]?key"\s*:\s*"[^"]*""#).expect("valid regex"),
                r#""api_key":"[REDACTED_AUTH]""#,
            ),
            (
                Regex::new(r#"(?i)bearer\s+[A-Za-z0-9._\-+/=]+"#).expect("valid regex"),
                "Bearer [REDACTED_AUTH]",
            ),
            (
                Regex::new(r"(?im)^(\s*authorization\s*:\s*).+$").expect("valid regex"),
                "${1}[REDACTED_AUTH]",
            ),
            (
                Regex::new(r"(?im)^(\s*x-api-key\s*:\s*).+$").expect("valid regex"),
                "${1}[REDACTED_AUTH]",
            ),
            (
                Regex::new(r"(?im)^(\s*api[_-]?key\s*:\s*).+$").expect("valid regex"),
                "${1}[REDACTED_AUTH]",
            ),
            (
                Regex::new(r"(?i)([?&]key=)[^&\s]+").expect("valid regex"),
                "${1}[REDACTED_AUTH]",
            ),
            // `ssh-keygen -l` output: redact the fingerprint and the key comment,
            // while retaining key size and algorithm for diagnostic value.
            (
                Regex::new(
                    r"(?im)^(\s*\d{3,4}\s+)SHA256:[A-Za-z0-9+/]{43}\s+[^\r\n]+?\s+(\((?:RSA|DSA|ECDSA|ED25519)\))\s*$",
                )
                .expect("valid regex"),
                "${1}[REDACTED_SSH_FINGERPRINT] [REDACTED_SSH_KEY_COMMENT] ${2}",
            ),
            // OpenSSH verbose key-offer lines identify the local key through a
            // path or comment immediately before its algorithm and fingerprint.
            (
                Regex::new(
                    r"(?im)^(\s*(?:debug\d+:\s*)?(?:Offering public key|Will attempt key|Server accepts key):\s+)[^\r\n]+?\s+((?:RSA|DSA|ECDSA|ED25519)(?:-SK)?\s+)SHA256:[A-Za-z0-9+/]{43}([^\r\n]*)$",
                )
                .expect("valid regex"),
                "${1}[REDACTED_SSH_KEY_COMMENT] ${2}[REDACTED_SSH_FINGERPRINT]${3}",
            ),
            // A serialized OpenSSH public key may carry an arbitrary comment
            // after its base64 material. Preserve the public key, redact only
            // the comment that commonly names a person, host, or key role.
            (
                Regex::new(
                    r"(?im)^(\s*(?:ssh-(?:rsa|dss|ed25519)|ecdsa-sha2-nistp(?:256|384|521)|sk-(?:ssh-ed25519|ecdsa-sha2-nistp256)@openssh\.com)\s+[A-Za-z0-9+/]{20,}={0,3})\s+[^\r\n]+$",
                )
                .expect("valid regex"),
                "${1} [REDACTED_SSH_KEY_COMMENT]",
            ),
            // Host redaction is deliberately limited to canonical OpenSSH
            // diagnostic sentences. A general hostname regex would erase
            // repository prose, model names, paths, and other useful records.
            (
                Regex::new(
                    r"(?im)^(\s*(?:debug\d+:\s*)?Connecting to\s+)\S+(?:\s+\[[^\]\r\n]+\])?(\s+port\s+\d+\.)\s*$",
                )
                .expect("valid regex"),
                "${1}[REDACTED_SSH_HOST]${2}",
            ),
            (
                Regex::new(
                    r"(?im)^(\s*(?:debug\d+:\s*)?Authenticating to\s+)\S+(\s+as\s+[^\r\n]+)$",
                )
                .expect("valid regex"),
                "${1}[REDACTED_SSH_HOST]${2}",
            ),
            (
                Regex::new(
                    r"(?im)^(\s*Authenticated to\s+)\S+(?:\s+\([^\r\n)]+\))?(\.)\s*$",
                )
                .expect("valid regex"),
                "${1}[REDACTED_SSH_HOST]${2}",
            ),
            (
                Regex::new(r"SHA256:[A-Za-z0-9+/]{43}=").expect("valid regex"),
                "[REDACTED_SSH_FINGERPRINT]",
            ),
            (
                Regex::new(r"SHA256:[A-Za-z0-9+/]{43}\b").expect("valid regex"),
                "[REDACTED_SSH_FINGERPRINT]",
            ),
            (
                Regex::new(r"sk-[A-Za-z0-9_\-]{20,}").expect("valid regex"),
                "[REDACTED_SECRET]",
            ),
            (
                Regex::new(r"AIza[0-9A-Za-z_\-]{35}").expect("valid regex"),
                "[REDACTED_SECRET]",
            ),
            (
                Regex::new(r"glpat-[A-Za-z0-9_\-]{20,}").expect("valid regex"),
                "[REDACTED_SECRET]",
            ),
            (
                Regex::new(r"github_pat_[A-Za-z0-9_]{22,}").expect("valid regex"),
                "[REDACTED_SECRET]",
            ),
            (
                Regex::new(r"gh[opsur]_[A-Za-z0-9]{36,}").expect("valid regex"),
                "[REDACTED_SECRET]",
            ),
            (
                Regex::new(r"AKIA[0-9A-Z]{16}").expect("valid regex"),
                "[REDACTED_SECRET]",
            ),
            (
                Regex::new(r#"(?i)("aws[_-]?secret[_-]?access[_-]?key"\s*:\s*")[^"]*""#)
                    .expect("valid regex"),
                "${1}[REDACTED_SECRET]\"",
            ),
            (
                Regex::new(r"(?i)\b(aws[_-]?secret[_-]?access[_-]?key\s*=\s*)[^&\s]+")
                    .expect("valid regex"),
                "${1}[REDACTED_SECRET]",
            ),
            (
                Regex::new(r"npm_[A-Za-z0-9]{36}").expect("valid regex"),
                "[REDACTED_SECRET]",
            ),
            (
                Regex::new(r"([A-Za-z][A-Za-z0-9+.\-]*://[^/\s:@?#]+:)[^@\s/?#]+(@)")
                    .expect("valid regex"),
                "${1}[REDACTED_SECRET]${2}",
            ),
            (
                Regex::new(r"ghp_[A-Za-z0-9]{36}").expect("valid regex"),
                "[REDACTED_SECRET]",
            ),
            (
                Regex::new(r"xox[baprs]-[A-Za-z0-9\-]{10,}").expect("valid regex"),
                "[REDACTED_SECRET]",
            ),
        ];
        Self { patterns }
    }

    /// HTTP defaults plus a bare `sk-…` token pattern suitable for scrubbing
    /// CLI argv where a provider key occasionally ends up as a flag value.
    pub fn with_argv_secrets() -> Self {
        let mut me = Self::http_default();
        me.patterns.push((
            Regex::new(r"sk-[A-Za-z0-9_\-]+").expect("valid regex"),
            "[REDACTED_API_KEY]",
        ));
        me
    }

    pub fn empty() -> Self {
        Self { patterns: vec![] }
    }

    /// Apply all patterns in order to `input`.
    pub fn apply_str(&self, input: &str) -> String {
        let mut out: Cow<'_, str> = Cow::Borrowed(input);
        for (pattern, replacement) in &self.patterns {
            match pattern.replace_all(&out, *replacement) {
                Cow::Borrowed(_) => {}
                Cow::Owned(new) => out = Cow::Owned(new),
            }
        }
        out.into_owned()
    }

    /// Byte-level convenience for callers holding raw HTTP bodies. Non-UTF-8
    /// input is returned unchanged.
    pub fn apply_bytes(&self, bytes: &[u8]) -> Vec<u8> {
        match std::str::from_utf8(bytes) {
            Ok(text) => self.apply_str(text).into_bytes(),
            Err(_) => bytes.to_vec(),
        }
    }
}

impl Default for PatternRedactor {
    fn default() -> Self {
        Self::http_default()
    }
}

// ---------------------------------------------------------------------------
// Combined
// ---------------------------------------------------------------------------

/// Apply env-value and default pattern redaction in one pass. Use when the
/// input shape is unknown (knowledge records, log lines, aggregated errors).
pub fn redact_all(input: &str) -> String {
    let env_scrubbed = redact_sensitive_env_text(input);
    default_pattern_redactor().apply_str(&env_scrubbed)
}

/// Return true when `input` is exactly one high-confidence credential token.
///
/// Callers that persist free-text artifacts can reject these whole-token values
/// instead of merely masking them; embedded occurrences are still handled by
/// [`redact_all`].
pub fn is_high_confidence_single_token_credential(input: &str) -> bool {
    let trimmed = input.trim();
    if trimmed.is_empty() || trimmed.split_whitespace().count() != 1 {
        return false;
    }
    high_confidence_single_token_patterns()
        .iter()
        .any(|pattern| pattern.is_match(trimmed))
}

pub(crate) fn default_pattern_redactor() -> &'static PatternRedactor {
    DEFAULT_PATTERN_REDACTOR.get_or_init(PatternRedactor::http_default)
}

fn high_confidence_single_token_patterns() -> &'static [Regex] {
    HIGH_CONFIDENCE_SINGLE_TOKEN_PATTERNS
        .get_or_init(|| {
            vec![
                Regex::new(r"^sk-[A-Za-z0-9_\-]{20,}$").expect("valid regex"),
                Regex::new(r"^AIza[0-9A-Za-z_\-]{35}$").expect("valid regex"),
                Regex::new(r"^glpat-[A-Za-z0-9_\-]{20,}$").expect("valid regex"),
                Regex::new(r"^github_pat_[A-Za-z0-9_]{22,}$").expect("valid regex"),
                Regex::new(r"^gh[opsur]_[A-Za-z0-9]{36,}$").expect("valid regex"),
                Regex::new(r"^AKIA[0-9A-Z]{16}$").expect("valid regex"),
                Regex::new(r"(?i)^aws[_-]?secret[_-]?access[_-]?key=[^&\s]+$")
                    .expect("valid regex"),
                Regex::new(r"^npm_[A-Za-z0-9]{36}$").expect("valid regex"),
                Regex::new(
                    r"^[A-Za-z][A-Za-z0-9+.\-]*://[^/\s:@?#]+:[^@\s/?#]+@[^/\s?#]+(?:[/?#]\S*)?$",
                )
                .expect("valid regex"),
                Regex::new(r"^ghp_[A-Za-z0-9]{36}$").expect("valid regex"),
                Regex::new(r"^xox[baprs]-[A-Za-z0-9\-]{10,}$").expect("valid regex"),
            ]
        })
        .as_slice()
}
