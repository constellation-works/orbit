//! Projects Grok CLI JSON wrappers onto their final answer text.
//!
//! `grok --output-format json` returns one wrapper object. Its `text` field
//! is the model's final answer; `thought`, tool metadata, and usage are
//! invocation telemetry and must not provide Orbit completion evidence.

use serde_json::Value;

/// Return the final answer when `stdout` is a recognized Grok JSON wrapper.
///
/// `None` keeps the legacy provider-agnostic path for direct Orbit envelopes
/// and multi-document output. A recognized wrapper with missing or malformed
/// `text` returns empty bytes so unrelated wrapper fields cannot take over.
pub(crate) fn project_grok_response(stdout: &[u8]) -> Option<Vec<u8>> {
    let wrapper = serde_json::from_slice::<Value>(stdout).ok()?;
    let wrapper = wrapper.as_object()?;

    if !wrapper.contains_key("text") && !wrapper.contains_key("stopReason") {
        return None;
    }

    Some(
        wrapper
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .as_bytes()
            .to_vec(),
    )
}
