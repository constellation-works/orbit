use std::fmt::{Display, Formatter};

use super::caller::{CallerCapabilities, CallerProvenance, CapabilityResolution};
use super::catalog::{GovernedOperation, PLUGIN_TOOL_READ_ONLY};
use super::env::OPERATOR_OVERRIDE_ENV;

/// A refused governed operation.
///
/// Carries the structured facts a caller needs to act — what was refused, what
/// it required, what the caller actually held, and the one documented way to
/// proceed — so no surface has to reconstruct the message from a string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizationDenial {
    /// The refused operation.
    pub operation: &'static GovernedOperation,
    /// What the caller held at the moment of refusal.
    pub granted: String,
    /// How those grants were derived.
    pub provenance: CallerProvenance,
    /// Which signals the refusing surface honored, and therefore which remedy
    /// the caller actually has.
    pub resolution: CapabilityResolution,
    /// Caller label an SSH-originated session forwarded, for attribution in
    /// the denial's log and audit row.
    pub remote_caller_machine_id: Option<String>,
}

impl AuthorizationDenial {
    /// The one documented way for this caller, on this surface, to proceed.
    ///
    /// The remedy is surface-specific because [`OPERATOR_OVERRIDE_ENV`] is
    /// deliberately ignored under [`CapabilityResolution::SessionOnly`];
    /// advising it there would send an operator in a circle.
    ///
    /// A session that arrived over SSH gets the same session-only remedy as a
    /// local one, and it is reachable: the destination honors the authority in
    /// the argv the caller's federated server composed, so serving the
    /// *calling* side with `--operator` raises it [ORB-12564].
    fn remedy(&self) -> String {
        if self.operation.id == PLUGIN_TOOL_READ_ONLY.id {
            return "This read-only tool needs a named caller. Use the local CLI or an MCP session granted the agent capability.".to_string();
        }
        match self.resolution {
            CapabilityResolution::ProcessEnvelope => format!(
                "If this is a deliberate operator action, re-run it with {OPERATOR_OVERRIDE_ENV}=1 \
                 set — the override is recorded in the audit trail."
            ),
            CapabilityResolution::SessionOnly => format!(
                "This MCP session's capabilities come from the server process it was served by, \
                 and {OPERATOR_OVERRIDE_ENV} in that process's environment is deliberately \
                 ignored. To perform this as an operator, serve the session from a server \
                 started as `orbit mcp serve --operator` — including a federated or remote-proxy \
                 server on the calling machine, whose `--operator` propagates into the argv every \
                 SSH destination is started with — or run the operation from the CLI."
            ),
        }
    }
}

impl Display for AuthorizationDenial {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "operation '{operation}' requires the `{required}` capability ({rationale}); \
             this caller was resolved as `{provenance}` holding [{granted}]. {remedy}",
            operation = self.operation.id,
            required = self.operation.allowed_label(),
            rationale = self.operation.rationale,
            provenance = self.provenance,
            granted = self.granted,
            remedy = self.remedy(),
        )
    }
}

/// Decide whether `caller` may perform `operation`.
///
/// This is the single decision function. Both chokepoints — the tool path in
/// `orbit-core` and the CLI command path in `orbit-cli` — call it and neither
/// reimplements any part of the rule.
pub fn authorize(
    operation: &'static GovernedOperation,
    caller: &CallerCapabilities,
) -> Result<(), AuthorizationDenial> {
    if operation.satisfied_by(&caller.grants) {
        return Ok(());
    }
    Err(AuthorizationDenial {
        operation,
        granted: caller.grants_label(),
        provenance: caller.provenance,
        resolution: caller.resolution,
        remote_caller_machine_id: caller.remote_caller_machine_id.clone(),
    })
}
