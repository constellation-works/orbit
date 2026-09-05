use crate::providers::common::render_prompt_with_embedded_envelope;
use crate::types::response_envelope_json_schema_arg;
use orbit_types::identity::ReasoningEffort;

/// Per-request command construction for Antigravity CLI (`agy`).
///
/// Static headless flags live on the shipped executor. This transport adds
/// only the model, effort, and generated envelope schema for one turn.
/// [ORB-11299]
pub(crate) struct AntigravityCliTransport {
    model: Option<String>,
    reasoning_effort: Option<ReasoningEffort>,
}

impl AntigravityCliTransport {
    pub(crate) fn new(model: Option<String>, reasoning_effort: Option<ReasoningEffort>) -> Self {
        Self {
            model,
            reasoning_effort,
        }
    }

    pub(crate) fn args(&self) -> Vec<String> {
        let mut args = Vec::new();
        args.push("--json-schema".to_string());
        args.push(response_envelope_json_schema_arg());
        if let Some(model) = &self.model {
            args.push("--model".to_string());
            args.push(model.clone());
        }
        if let Some(effort) = self.reasoning_effort {
            args.push("--effort".to_string());
            args.push(effort.to_string());
        }
        args
    }

    /// Documented stream-json stdin: one `user` event, then the caller closes
    /// the pipe. Putting the envelope on `-p` would leak task context into
    /// process listings and audit argv.
    pub(crate) fn stdin(&self, envelope_json: &[u8]) -> Vec<u8> {
        let prompt = render_prompt_with_embedded_envelope(envelope_json);
        let prompt = String::from_utf8_lossy(&prompt);
        let event = serde_json::json!({
            "event": "user",
            "message": { "content": prompt.as_ref() },
        });
        // String content serializes to a JSON object; this cannot fail.
        let mut bytes = serde_json::to_vec(&event)
            .unwrap_or_else(|_| br#"{"event":"user","message":{"content":""}}"#.to_vec());
        bytes.push(b'\n');
        bytes
    }

    pub(crate) fn model_name(&self) -> Option<&str> {
        self.model.as_deref()
    }
}
