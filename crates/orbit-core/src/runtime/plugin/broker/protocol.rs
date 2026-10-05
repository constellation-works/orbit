//! Broker wire protocol (design §4.3): one length-prefixed JSON request and
//! one length-prefixed JSON response per connection.
//!
//! A frame is a 4-byte big-endian body length followed by that many bytes of
//! UTF-8 JSON. The length is checked against the cap before any body byte is
//! read, so an oversized request is refused without being buffered.

use std::io::{self, Read, Write};
use std::path::PathBuf;

use orbit_common::{NotFoundKind, OrbitError};
use serde_json::{Value, json};

/// Protocol version every frame carries.
pub(crate) const SCHEMA_VERSION: u64 = 1;

/// Largest request body the broker reads.
pub(crate) const MAX_REQUEST_BYTES: u32 = 4 * 1024 * 1024;

/// More requests than the broker runs and queues at once. Retryable.
pub(crate) const BUSY: &str = "plugin_broker_busy";
/// The request frame declared a body over [`MAX_REQUEST_BYTES`].
pub(crate) const REQUEST_TOO_LARGE: &str = "plugin_broker_request_too_large";
/// The request body is not a version-1 request object.
pub(crate) const INVALID_REQUEST: &str = "plugin_broker_invalid_request";
/// The run does not authorize this call: the tool is neither a plugin tool
/// nor one of the broker's read-only `github.*` built-ins, its activity policy
/// or the plugin's grants refuse it, or the request names a cwd or workspace
/// outside the run.
const REFUSED: &str = "plugin_broker_refused";
/// The tool's input does not satisfy its schema.
const INVALID_INPUT: &str = "plugin_broker_invalid_input";
/// The call was authorized but could not complete: the backend failed,
/// timed out, or answered with something other than its envelope.
const CALL_FAILED: &str = "plugin_broker_call_failed";

/// Why a request frame could not be read.
#[derive(Debug)]
pub(crate) enum FrameError {
    /// The declared body length exceeds the cap. No body byte was read.
    TooLarge { declared: u32 },
    /// The peer closed, timed out, or sent a short frame.
    Io(io::Error),
}

/// Read one frame whose body is at most `limit` bytes.
pub(crate) fn read_frame(reader: &mut impl Read, limit: u32) -> Result<Vec<u8>, FrameError> {
    let mut header = [0u8; 4];
    reader.read_exact(&mut header).map_err(FrameError::Io)?;
    let declared = u32::from_be_bytes(header);
    if declared > limit {
        return Err(FrameError::TooLarge { declared });
    }
    let mut body = vec![0u8; declared as usize];
    reader.read_exact(&mut body).map_err(FrameError::Io)?;
    Ok(body)
}

/// Write one frame.
pub(crate) fn write_frame(writer: &mut impl Write, body: &[u8]) -> io::Result<()> {
    let declared = u32::try_from(body.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "frame body too large"))?;
    writer.write_all(&declared.to_be_bytes())?;
    writer.write_all(body)?;
    writer.flush()
}

/// A structured broker refusal, rendered as the §4.3 error response.
pub(crate) fn error_response(code: &str, message: &str, retryable: bool) -> Vec<u8> {
    json!({
        "schema_version": SCHEMA_VERSION,
        "ok": false,
        "error": {
            "code": code,
            "message": message,
            "retryable": retryable,
            "detail": Value::Null,
        },
    })
    .to_string()
    .into_bytes()
}

/// Where the nested `orbit` received the call. A hint for the audit row;
/// it grants nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EntryPoint {
    Cli,
    Mcp,
}

/// A version-1 request (design §4.3): only what the caller legitimately
/// chooses. Nothing here decides the run, task, allowlist or profile, which
/// come from the host's own dispatch record.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct BrokerRequest {
    pub(crate) tool: String,
    pub(crate) input: Value,
    /// Where the caller runs; the broker requires it to lie within the run's
    /// worktree.
    pub(crate) cwd: PathBuf,
    /// A workspace selector, which may name only the run's own workspace.
    pub(crate) workspace: Option<String>,
    pub(crate) entry_point: EntryPoint,
    pub(crate) dry_run: bool,
}

/// Parse and validate a request body. `schema_version`, `tool`, `input` and
/// an absolute `cwd` are required; `workspace` defaults to none,
/// `entry_point` to `cli` and `dry_run` to false.
pub(crate) fn parse_request(body: &[u8]) -> Result<BrokerRequest, String> {
    let request: Value =
        serde_json::from_slice(body).map_err(|error| format!("request is not JSON: {error}"))?;
    let object = request
        .as_object()
        .ok_or_else(|| "request must be a JSON object".to_string())?;
    match object.get("schema_version").and_then(Value::as_u64) {
        Some(SCHEMA_VERSION) => {}
        other => {
            return Err(format!(
                "unsupported schema_version {}; this broker speaks {SCHEMA_VERSION}",
                other.map_or_else(|| "(missing)".to_string(), |value| value.to_string())
            ));
        }
    }
    let tool = object
        .get("tool")
        .and_then(Value::as_str)
        .filter(|tool| !tool.is_empty())
        .ok_or_else(|| "request must name a `tool`".to_string())?;
    let input = object
        .get("input")
        .filter(|input| input.is_object())
        .ok_or_else(|| "request `input` must be a JSON object".to_string())?;
    let cwd = object
        .get("cwd")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .filter(|cwd| cwd.is_absolute())
        .ok_or_else(|| "request `cwd` must be an absolute path".to_string())?;
    let workspace = match object.get("workspace") {
        None | Some(Value::Null) => None,
        Some(Value::String(workspace)) if !workspace.is_empty() => Some(workspace.clone()),
        Some(_) => return Err("request `workspace` must be a string or null".to_string()),
    };
    let entry_point = match object.get("entry_point") {
        None => EntryPoint::Cli,
        Some(value) => match value.as_str() {
            Some("cli") => EntryPoint::Cli,
            Some("mcp") => EntryPoint::Mcp,
            _ => return Err("request `entry_point` must be \"cli\" or \"mcp\"".to_string()),
        },
    };
    let dry_run = match object.get("dry_run") {
        None => false,
        Some(value) => value
            .as_bool()
            .ok_or_else(|| "request `dry_run` must be a boolean".to_string())?,
    };
    Ok(BrokerRequest {
        tool: tool.to_string(),
        input: input.clone(),
        cwd,
        workspace,
        entry_point,
        dry_run,
    })
}

/// A call's result, rendered as the §4.3 success response.
pub(crate) fn output_response(output: Value) -> Vec<u8> {
    json!({
        "schema_version": SCHEMA_VERSION,
        "ok": true,
        "output": output,
    })
    .to_string()
    .into_bytes()
}

/// A call's error as the §4.3 error response. A backend's own structured
/// error keeps its code, retryability and detail; the host's refusals and
/// failures map onto the broker's codes, none of them retryable.
pub(crate) fn call_error_response(error: &OrbitError) -> Vec<u8> {
    let code = match error {
        OrbitError::RemoteTool {
            code,
            message,
            payload,
        } => {
            return json!({
                "schema_version": SCHEMA_VERSION,
                "ok": false,
                "error": {
                    "code": payload.get("code").and_then(Value::as_str).unwrap_or(code),
                    "message": payload.get("message").and_then(Value::as_str).unwrap_or(message),
                    "retryable": payload.get("retryable").and_then(Value::as_bool).unwrap_or(false),
                    "detail": payload.get("detail").cloned().unwrap_or(Value::Null),
                },
            })
            .to_string()
            .into_bytes();
        }
        OrbitError::PolicyDenied(_)
        | OrbitError::CapabilityDenied(_)
        | OrbitError::CapabilityRefused(_)
        | OrbitError::PluginDisabledInWorkspace { .. }
        | OrbitError::PluginDisabledOnHost { .. }
        | OrbitError::PluginBuildConsentRequired(_)
        | OrbitError::PluginBuildConsentUnavailable(_)
        | OrbitError::PluginBuildFetchUnsupported(_)
        | OrbitError::NotFound {
            kind: NotFoundKind::Tool,
            ..
        } => REFUSED,
        OrbitError::InvalidInput(_) | OrbitError::InvalidInputDiagnostic { .. } => INVALID_INPUT,
        _ => CALL_FAILED,
    };
    error_response(code, &error.to_string(), false)
}
