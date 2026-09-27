//! Broker wire protocol (design §4.3): one length-prefixed JSON request and
//! one length-prefixed JSON response per connection.
//!
//! A frame is a 4-byte big-endian body length followed by that many bytes of
//! UTF-8 JSON. The length is checked against the cap before any body byte is
//! read, so an oversized request is refused without being buffered.

use std::io::{self, Read, Write};

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
/// An authenticated, well-formed request the broker cannot execute yet: plugin
/// call forwarding lands in a later slice (design §8, step 3).
pub(crate) const NOT_IMPLEMENTED: &str = "not_implemented";

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

/// Validate a request body's envelope. The tool name is the only field read
/// here; nothing in a request chooses the run, task, workspace authority,
/// allowlist or profile, which come from the host's own dispatch record.
pub(crate) fn parse_request(body: &[u8]) -> Result<String, String> {
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
    object
        .get("tool")
        .and_then(Value::as_str)
        .filter(|tool| !tool.is_empty())
        .map(str::to_string)
        .ok_or_else(|| "request must name a `tool`".to_string())
}
