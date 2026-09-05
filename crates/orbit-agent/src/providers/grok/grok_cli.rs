use crate::providers::common::render_prompt_with_embedded_envelope;
use orbit_types::identity::ReasoningEffort;

pub(crate) struct GrokCliTransport {
    model: Option<String>,
    reasoning_effort: Option<ReasoningEffort>,
}

impl GrokCliTransport {
    pub(crate) fn new(model: Option<String>, reasoning_effort: Option<ReasoningEffort>) -> Self {
        Self {
            model,
            reasoning_effort,
        }
    }

    // Static Grok CLI flags live in the executor definition; this transport
    // only adds per-request toggles.
    pub(crate) fn args(&self) -> Vec<String> {
        let mut args = Vec::new();

        if let Some(model) = &self.model {
            args.push("--model".to_string());
            args.push(model.clone());
        }
        if let Some(effort) = self.reasoning_effort {
            args.push("--reasoning-effort".to_string());
            args.push(effort.to_string());
        }

        args
    }

    pub(crate) fn stdin(&self, envelope_json: &[u8]) -> Vec<u8> {
        render_prompt_with_embedded_envelope(envelope_json)
    }

    pub(crate) fn model_name(&self) -> Option<&str> {
        self.model.as_deref()
    }
}
