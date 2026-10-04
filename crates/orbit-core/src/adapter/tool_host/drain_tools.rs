//! Tool-boundary adapter for the distributed drain's owner surface.
//!
//! Every authority fact these handlers use comes from the trusted session
//! envelope the transport built, never from the tool payload: a caller cannot
//! name a machine to borrow its capabilities, and the MCP server and
//! `orbit tool run` reach the same application entry points, so the two
//! surfaces cannot drift into different lifecycle checks.

use orbit_common::OrbitError;
use orbit_common::protocol::tool_input::reject_unknown_tool_fields;
use orbit_store::contracts::{AdmissionRequest, ClaimMutation};
use orbit_types::tool::ToolSessionContext;
use serde_json::Value;

use crate::OrbitRuntime;
use crate::application::distributed::DeclaredCallerContract;

pub(super) fn probe(
    runtime: &OrbitRuntime,
    session: &ToolSessionContext,
    input: Value,
) -> Result<Value, OrbitError> {
    reject_unknown_tool_fields(
        &input,
        &[
            "caller_version",
            "caller_schema",
            "caller_review_policy",
            "workspace",
            "agent",
            "model",
        ],
    )?;
    let declared = DeclaredCallerContract {
        caller_version: optional_string(&input, "caller_version"),
        caller_schema: optional_u32(&input, "caller_schema")?,
        caller_review_policy: optional_string(&input, "caller_review_policy"),
    };
    let report = runtime.drain_probe(session, &declared)?;
    serde_json::to_value(report).map_err(|error| OrbitError::Store(error.to_string()))
}

pub(super) fn receipt_lookup(
    runtime: &OrbitRuntime,
    session: &ToolSessionContext,
    input: Value,
) -> Result<Value, OrbitError> {
    reject_unknown_tool_fields(
        &input,
        &[
            "request_id",
            "machine_id",
            "lookup_schema",
            "workspace",
            "agent",
            "model",
        ],
    )?;
    let request_id = orbit_tools::require_str(&input, "request_id")?;
    let lookup_schema = optional_u32(&input, "lookup_schema")?
        .unwrap_or(orbit_store::contracts::ADMISSION_RECEIPT_LOOKUP_SCHEMA);
    let machine_id = optional_string(&input, "machine_id");
    let lookup = runtime.lookup_admission_receipt(
        session,
        &request_id,
        machine_id.as_deref(),
        lookup_schema,
    )?;
    serde_json::to_value(lookup).map_err(|error| OrbitError::Store(error.to_string()))
}

/// `orbit.task.pull`. The request is the caller's durable admission request,
/// parsed whole so the owner's replay comparison sees exactly what was sent.
pub(super) fn pull(
    runtime: &OrbitRuntime,
    session: &ToolSessionContext,
    input: Value,
) -> Result<Value, OrbitError> {
    reject_unknown_tool_fields(
        &input,
        &[
            "request_id",
            "caller_version",
            "caller_schema",
            "caller_review_policy",
            "run_context",
            "ship",
            "crews",
            "workspace",
            "agent",
            "model",
        ],
    )?;
    let request: AdmissionRequest = parse_fields(
        &input,
        &[
            "request_id",
            "caller_version",
            "caller_schema",
            "caller_review_policy",
            "run_context",
            "ship",
            "crews",
        ],
    )?;
    let response = runtime.serve_task_pull(session, &request)?;
    serde_json::to_value(response).map_err(|error| OrbitError::Store(error.to_string()))
}

/// `orbit.drain.claim.bind`.
pub(super) fn claim_bind(
    runtime: &OrbitRuntime,
    session: &ToolSessionContext,
    input: Value,
) -> Result<Value, OrbitError> {
    reject_unknown_tool_fields(
        &input,
        &["claim_id", "run_id", "ship", "workspace", "agent", "model"],
    )?;
    let claim_id = orbit_tools::require_str(&input, "claim_id")?;
    let run_id = orbit_tools::require_str(&input, "run_id")?;
    let ship = parse_field(&input, "ship")?;
    let result = runtime.serve_claim_bind(session, &claim_id, &run_id, ship)?;
    serde_json::to_value(result).map_err(|error| OrbitError::Store(error.to_string()))
}

/// `orbit.drain.claim.settle`.
pub(super) fn claim_settle(
    runtime: &OrbitRuntime,
    session: &ToolSessionContext,
    input: Value,
) -> Result<Value, OrbitError> {
    reject_unknown_tool_fields(
        &input,
        &[
            "claim_id",
            "run_id",
            "settlement",
            "workspace",
            "agent",
            "model",
        ],
    )?;
    let claim_id = orbit_tools::require_str(&input, "claim_id")?;
    let run_id = optional_string(&input, "run_id");
    let settlement: ClaimMutation = parse_field(&input, "settlement")?;
    let result = runtime.serve_claim_settle(session, &claim_id, run_id.as_deref(), settlement)?;
    serde_json::to_value(result).map_err(|error| OrbitError::Store(error.to_string()))
}

fn parse_field<T: serde::de::DeserializeOwned>(input: &Value, key: &str) -> Result<T, OrbitError> {
    let value = input
        .get(key)
        .cloned()
        .ok_or_else(|| OrbitError::InvalidInput(format!("`{key}` is required")))?;
    serde_json::from_value(value)
        .map_err(|error| OrbitError::InvalidInput(format!("`{key}`: {error}")))
}

/// Deserialize the named fields together as one value, ignoring the routing
/// and attribution envelope fields every tool accepts.
fn parse_fields<T: serde::de::DeserializeOwned>(
    input: &Value,
    keys: &[&str],
) -> Result<T, OrbitError> {
    let object: serde_json::Map<String, Value> = keys
        .iter()
        .filter_map(|key| {
            input
                .get(*key)
                .map(|value| ((*key).to_string(), value.clone()))
        })
        .collect();
    serde_json::from_value(Value::Object(object))
        .map_err(|error| OrbitError::InvalidInput(error.to_string()))
}

pub(super) fn claims(runtime: &OrbitRuntime, input: Value) -> Result<Value, OrbitError> {
    reject_unknown_tool_fields(&input, &["workspace", "agent", "model"])?;
    Ok(Value::Array(runtime.inspect_distributed_claims()?))
}

fn optional_string(input: &Value, key: &str) -> Option<String> {
    input
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

/// A malformed version field is invalid input, not a compatibility comparison.
fn optional_u32(input: &Value, key: &str) -> Result<Option<u32>, OrbitError> {
    match input.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(number)) => number
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
            .map(Some)
            .ok_or_else(|| invalid_version_field(key)),
        Some(Value::String(raw)) => raw
            .trim()
            .parse::<u32>()
            .map(Some)
            .map_err(|_| invalid_version_field(key)),
        Some(_) => Err(invalid_version_field(key)),
    }
}

fn invalid_version_field(key: &str) -> OrbitError {
    OrbitError::InvalidInput(format!("`{key}` must be a non-negative integer version"))
}
