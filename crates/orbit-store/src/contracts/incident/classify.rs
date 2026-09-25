//! Pass 1: classify a failed row as denied, expected, diagnostic or
//! unexpected, and derive the surface it failed on.

use super::types::{FAILURE_ONLY_DIAGNOSTIC_SURFACES, FailureClass};
use orbit_types::telemetry::{AuditEvent, AuditEventStatus};

/// Message fragments that mark a documented negative path. Each mirrors an
/// `OrbitError` variant's `Display` rendering in `orbit-common`, which every
/// surface funnels its errors through before they reach `error_message`.
/// Matching is lowercase substring, so a wrapped/prefixed message still
/// classifies.
const EXPECTED_FAILURE_MARKERS: &[&str] = &[
    "invalid input",
    "not found",
    "already exists",
    "validation failed",
    "sensitive input rejected",
    "invalid status transition",
    "unsupported",
    "not local to the current worktree",
    "artifact unavailable",
    "companion not installed",
];

/// Message fragments that mark a refusal rather than a failure. A refusal
/// recorded with `status = failure` (some surfaces translate late) still
/// classifies as a denial so the two populations do not blur.
const DENIAL_MARKERS: &[&str] = &["policy denied", "capability denied", "permission denied"];

/// Classifies one failed audit row. Denial status wins outright; otherwise the
/// `OrbitError`-derived markers decide whether this was a documented negative
/// path. Anything unmatched is treated as unexpected — the conservative
/// direction, since under-reporting a real failure is the costlier mistake.
pub fn classify(event: &AuditEvent) -> FailureClass {
    if is_lifecycle_diagnostic(event) {
        return FailureClass::Diagnostic;
    }
    if matches!(event.status, AuditEventStatus::Denied) {
        return FailureClass::Denied;
    }
    let message = event
        .error_message
        .as_deref()
        .unwrap_or_default()
        .to_lowercase();
    if message.is_empty() {
        return FailureClass::Unexpected;
    }
    if DENIAL_MARKERS.iter().any(|marker| message.contains(marker)) {
        return FailureClass::Denied;
    }
    if EXPECTED_FAILURE_MARKERS
        .iter()
        .any(|marker| message.contains(marker))
    {
        return FailureClass::Expected;
    }
    FailureClass::Unexpected
}

/// True for a failure-only diagnostic surface. Callers use the same predicate
/// when excluding named rows from callable-tool rate rankings.
pub fn is_failure_only_diagnostic_surface(name: &str) -> bool {
    let name = name.trim();
    FAILURE_ONLY_DIAGNOSTIC_SURFACES.contains(&name)
}

/// True when an audit row is a lifecycle diagnostic rather than a call whose
/// success and failure populations can be compared.
pub fn is_lifecycle_diagnostic(event: &AuditEvent) -> bool {
    is_failure_only_diagnostic_surface(&surface_of(event))
}

/// True when the row names a real tool. Direct CLI and legacy job-run
/// lifecycle rows have empty/`NULL` tool names; neither is a tool called
/// `unknown`.
pub fn has_tool_identity(event: &AuditEvent) -> bool {
    event
        .tool_name
        .as_deref()
        .is_some_and(|name| !name.trim().is_empty())
}

/// The reporting surface of an audit row: its tool name when it has one,
/// otherwise its `command`/`subcommand` pair. Never empty.
pub(super) fn surface_of(event: &AuditEvent) -> String {
    if let Some(tool) = event.tool_name.as_deref().filter(|name| !name.is_empty()) {
        return tool.to_string();
    }
    match event.subcommand.as_deref().filter(|sub| !sub.is_empty()) {
        Some(sub) if !event.command.is_empty() => format!("{} {sub}", event.command),
        Some(sub) => sub.to_string(),
        None if !event.command.is_empty() => event.command.clone(),
        None => "unknown".to_string(),
    }
}
