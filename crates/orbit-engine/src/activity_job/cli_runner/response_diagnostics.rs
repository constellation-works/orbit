//! Bounded, redacted step messages for provider response and completion
//! failures.

use orbit_common::security::redaction::{PatternRedactor, redact_sensitive_env_text};

const RESPONSE_DIAGNOSTIC_LIMIT_CHARS: usize = 1024;

/// Append the Orbit-owned write-denial attribution to a step message that was
/// classified from the protocol frame alone.
///
/// A provider that exits 0 after a policy-denied write yields a frame-shaped
/// message ("no terminating envelope") that says nothing about *why* the agent
/// stopped. The sandbox diagnostic is the only text in the system that names
/// the attempted path and the rule that shadowed it, so it rides along rather
/// than replacing the frame classification — the step still failed for the
/// protocol reason, and the denial is the cause worth acting on.
///
/// `diagnostic` is already the `linux_bwrap_write_grant_diagnostic` string
/// (bounded and redacted); this deliberately does not reformat it, so operators
/// and greps see one message format for a write denial regardless of which
/// branch surfaced it.
// pub(super) widened for tests/ layout under ORB-00225; test reaches via exposed surface.
pub(super) fn with_sandbox_write_attribution(message: String, diagnostic: Option<&str>) -> String {
    match diagnostic {
        Some(diagnostic) => format!("{message} {diagnostic}"),
        None => message,
    }
}

pub(super) fn response_diagnostic(error: &str, redactor: &PatternRedactor) -> String {
    format!(
        "cli response envelope invalid: {}",
        bounded_diagnostic(error, redactor)
    )
}

/// [ORB-10449] Name the protocol violation for what it is. The old surfaced
/// failure was whatever deterministic gate tripped several steps later, which
/// reads as a downstream defect; this says the agent stopped before finishing
/// its turn and points at the evidence.
pub(super) fn completion_diagnostic(error: &str, redactor: &PatternRedactor) -> String {
    format!(
        "agent step did not complete: the provider exited 0 but stdout carried no valid \
         terminating Orbit response envelope ({}). The invocation ended without finishing its \
         contract — typically an agent that yielded mid-work — so this step's work is incomplete \
         and only what it persisted before stopping is durable.",
        bounded_diagnostic(error, redactor)
    )
}

/// The step message for an envelope that declared failure. [ORB-14260] An
/// agent whose claimed-owner call could not reach its coordinator declares
/// `owner_route_unavailable`; the typed marker lets recovery skip the step and
/// a pull drain release the claim. The code grants no authority: the marker
/// only withholds a repair and returns the task to its owner's backlog.
pub(super) fn declared_failure_diagnostic(
    status: &str,
    failure: Option<&orbit_agent::DeclaredResponseFailure>,
    redactor: &PatternRedactor,
) -> String {
    let prefix =
        format!("cli subprocess reported declared envelope status={status:?} despite exit 0");
    let Some(error) = failure.and_then(|failure| failure.error.as_ref()) else {
        return format!("{prefix}: declared envelope error details unavailable");
    };
    let marker = if orbit_types::workflow::is_owner_route_unavailable(Some(&error.code), None) {
        format!("{} ", orbit_types::workflow::OWNER_ROUTE_UNAVAILABLE_MARKER)
    } else {
        String::new()
    };

    format!(
        "{marker}{prefix}: error.code={}; error.message={}",
        bounded_diagnostic(&error.code, redactor),
        bounded_diagnostic(&error.message, redactor),
    )
}

pub(super) fn bounded_diagnostic(error: &str, redactor: &PatternRedactor) -> String {
    let redacted = redactor.apply_str(&redact_sensitive_env_text(error));
    let bounded: String = redacted
        .chars()
        .take(RESPONSE_DIAGNOSTIC_LIMIT_CHARS)
        .collect();
    let suffix = if bounded.len() < redacted.len() {
        "…"
    } else {
        ""
    };
    format!("{bounded}{suffix}")
}
