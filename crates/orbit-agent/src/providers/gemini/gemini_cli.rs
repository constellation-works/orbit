use crate::providers::common::render_prompt_with_embedded_envelope;

pub(crate) struct GeminiCliTransport {
    model: Option<String>,
}

impl GeminiCliTransport {
    pub(crate) fn new(model: Option<String>) -> Self {
        Self { model }
    }

    // Static Gemini CLI flags live in the executor definition; this transport
    // only adds the configured model.
    pub(crate) fn args(&self) -> Vec<String> {
        let mut args = Vec::new();

        if let Some(model) = &self.model {
            args.push("-m".to_string());
            args.push(model.clone());
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
