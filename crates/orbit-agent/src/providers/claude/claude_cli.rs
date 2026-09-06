use crate::providers::common::render_prompt_with_embedded_envelope;
use crate::types::response_envelope_json_schema_arg;
use orbit_types::identity::ReasoningEffort;

fn claude_cli_model_arg(model: &str) -> String {
    let trimmed = model.trim();
    if let Some(version) = trimmed.strip_prefix("opus-") {
        return format!("claude-opus-{}", version.replace('.', "-"));
    }
    if let Some(version) = trimmed.strip_prefix("sonnet-") {
        return format!("claude-sonnet-{}", version.replace('.', "-"));
    }
    if let Some(version) = trimmed.strip_prefix("fable-") {
        return format!("claude-fable-{}", version.replace('.', "-"));
    }
    trimmed.to_string()
}

pub(crate) struct ClaudeCliTransport {
    model: Option<String>,
    reasoning_effort: Option<ReasoningEffort>,
}

impl ClaudeCliTransport {
    pub(crate) fn new(model: Option<String>, reasoning_effort: Option<ReasoningEffort>) -> Self {
        Self {
            model,
            reasoning_effort,
        }
    }

    // Static Claude CLI flags live in the executor definition; this transport
    // only adds per-request toggles.
    pub(crate) fn args(&self, verbose: bool) -> Vec<String> {
        let mut args = Vec::new();

        // [ORB-10746] Structured output is what actually enforces the Orbit
        // response envelope; the prompt contract is guidance the model may
        // ignore, and in `jrun-20260812-0312-9` did.
        //
        // Emitted here rather than added to the two `claude.yaml` copies on
        // purpose: the schema is generated from one protocol definition, so a
        // per-request flag cannot drift between the packaged asset and the
        // installed workspace resource the way two hand-edited arg lists can.
        // A CLI without the flag rejects it at argv parse, before any agent
        // work runs — the failure Orbit wants, and the reason there is no
        // unconstrained fallback.
        args.push("--json-schema".to_string());
        args.push(response_envelope_json_schema_arg());

        if verbose {
            args.push("--verbose".to_string());
        }

        if let Some(model) = &self.model {
            args.push("--model".to_string());
            args.push(claude_cli_model_arg(model));
        }
        if let Some(effort) = self.reasoning_effort {
            args.push("--effort".to_string());
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
