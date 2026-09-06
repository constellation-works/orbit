#[path = "response/envelope.rs"]
// Visible to sibling-layout tests for this file-rooted module without
// reintroducing a nested `response/tests` tree.
pub(in crate::types) mod envelope;
#[path = "response/protocol_schema.rs"]
// Visible to sibling-layout tests for this file-rooted module without
// reintroducing a nested `response/tests` tree.
pub(in crate::types) mod protocol_schema;
#[path = "response/tool_calls.rs"]
mod tool_calls;
#[path = "response/trace.rs"]
mod trace;
#[path = "response/usage.rs"]
// Visible to sibling-layout tests for this file-rooted module without
// reintroducing a nested `response/tests` tree.
pub(in crate::types) mod usage;
#[path = "response/wrapper.rs"]
// Visible to sibling-layout tests for this file-rooted module without
// reintroducing a nested `response/tests` tree.
pub(in crate::types) mod wrapper;

use orbit_common::OrbitError;
use orbit_types::telemetry::ToolCallTrace;
use serde_json::Value;

pub use envelope::{
    DeclaredResponseFailure, is_timeout, parse_and_validate_response,
    peek_declared_response_failure, peek_response_status, response_envelope_protocol_check,
};
pub use protocol_schema::{response_envelope_json_schema, response_envelope_json_schema_arg};
pub use wrapper::provider_invocation_diagnostic;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentInvocationSpec {
    pub runtime_key: &'static str,
    pub program: String,
    pub args: Vec<String>,
    pub stdin: Vec<u8>,
    pub stdout_schema_json: Option<Value>,
    pub required_env_vars: &'static [&'static str],
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
