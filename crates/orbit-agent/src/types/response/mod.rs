pub(in crate::types) mod envelope;
pub(in crate::types) mod protocol_schema;
mod tool_calls;
mod trace;
pub(in crate::types) mod usage;
pub(in crate::types) mod wrapper;

use orbit_common::OrbitError;
use orbit_types::telemetry::ToolCallTrace;
use serde_json::Value;

#[cfg(test)]
pub use envelope::parse_and_validate_response;
pub use envelope::{DeclaredResponseFailure, ParsedStdout};
pub use protocol_schema::response_envelope_json_schema_arg;
pub use wrapper::provider_invocation_diagnostic;

/// Phrases a provider CLI prints — on stderr or as its terminal error — when
/// it cannot authenticate. Lowercase, matched case-insensitively [ORB-13941].
const PROVIDER_AUTH_FAILURE_PHRASES: &[&str] = &[
    "authentication failed",
    "authentication required",
    "authentication_error",
    "not authenticated",
    "unauthenticated",
    "not logged in",
    "please log in",
    "please login",
    "login required",
    "oauth session expired",
    "no authentication information found",
    "invalid api key",
    "invalid_api_key",
    "api key not valid",
];

/// Whether a provider's own failure text says it could not authenticate, so
/// the provider is unusable on this host until an operator signs it in again.
///
/// Pass only text the provider wrote about itself — its stderr, its terminal
/// error — never the agent's transcript, where a tool's own login failure
/// would read the same.
#[must_use]
pub fn provider_authentication_failure(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    PROVIDER_AUTH_FAILURE_PHRASES
        .iter()
        .any(|phrase| text.contains(phrase))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentInvocationSpec {
    pub runtime_key: &'static str,
    pub program: String,
    pub args: Vec<String>,
    pub stdin: Vec<u8>,
    pub stdout_schema_json: Option<Value>,
    pub required_env_vars: &'static [&'static str],
    /// Environment entries the provider pins for every invocation. They are
    /// applied over the `[execution.env]` allowlist, so an outer process
    /// cannot forward a different value.
    pub fixed_env: &'static [(&'static str, &'static str)],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentResponseStatus {
    Success,
    Failed,
    Timeout,
}

type ResponseParseResult = Result<
    (
        orbit_types::workflow::AgentResponseEnvelope,
        AgentResponseStatus,
        orbit_types::telemetry::InvocationTrace,
    ),
    OrbitError,
>;

type JsonMap = serde_json::Map<String, Value>;

#[derive(Default)]
struct ToolCallCollector {
    calls: Vec<ToolCallTrace>,
    by_id: std::collections::HashMap<String, usize>,
}
