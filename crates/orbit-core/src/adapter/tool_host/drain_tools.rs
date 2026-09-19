//! Tool-boundary adapter for the distributed drain's read-only surface.
//!
//! Every authority fact these handlers use comes from the trusted session
//! envelope the transport built, never from the tool payload: a caller cannot
//! name a machine to borrow its capabilities, and the MCP server and
//! `orbit tool run` reach the same application entry points, so the two
//! surfaces cannot drift into different lifecycle checks.

use orbit_common::OrbitError;
use orbit_common::protocol::tool_input::reject_unknown_tool_fields;
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
    OrbitError::InvalidInput(format!(
        "invalid_input: `{key}` must be a non-negative integer version"
    ))
}
