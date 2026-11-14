use orbit_common::OrbitError;

use crate::providers::common::render_prompt_with_embedded_envelope;

pub(crate) struct OllamaCliTransport {
    model: String,
}

impl OllamaCliTransport {
    pub(crate) fn new(model: Option<String>) -> Result<Self, OrbitError> {
        let model = model
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                OrbitError::InvalidInput(
                    "ollama provider requires a model; set step.model or configure tier mappings on the executor"
                        .to_string(),
                )
            })?;
        Ok(Self { model })
    }

    pub(crate) fn args(&self) -> Vec<String> {
        vec![
            "run".to_string(),
            self.model.clone(),
            "--format".to_string(),
            "json".to_string(),
        ]
    }

    pub(crate) fn stdin(&self, envelope_json: &[u8]) -> Vec<u8> {
        render_prompt_with_embedded_envelope(envelope_json)
    }

    pub(crate) fn model_name(&self) -> &str {
        &self.model
    }
}
