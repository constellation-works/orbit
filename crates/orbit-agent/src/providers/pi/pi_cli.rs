use orbit_types::identity::ReasoningEffort;

use crate::providers::common::render_prompt_with_embedded_envelope;

/// Per-request command construction for the Pi coding agent CLI. [ORB-11296]
///
/// The shipped executor owns the static headless flags (`--mode json`,
/// `--no-session`, `--no-approve`, `--offline`); this transport adds only what
/// Orbit's crew resolution decides per invocation.
pub(crate) struct PiCliTransport {
    model: Option<String>,
    reasoning_effort: Option<ReasoningEffort>,
}

impl PiCliTransport {
    pub(crate) fn new(model: Option<String>, reasoning_effort: Option<ReasoningEffort>) -> Self {
        Self {
            model,
            reasoning_effort,
        }
    }

    /// `--model` takes Pi's model *pattern* (which may carry a `provider/id`
    /// prefix), and `--thinking` takes the crew effort. Effort is rendered as
    /// its own flag rather than through Pi's `<model>:<thinking>` shorthand so
    /// the two crew fields stay independently readable in argv and audit
    /// records, and so a crew that sets only one of them cannot accidentally
    /// rewrite the other.
    pub(crate) fn args(&self) -> Vec<String> {
        let mut args = Vec::new();
        if let Some(model) = &self.model {
            args.push("--model".to_string());
            args.push(model.clone());
        }
        if let Some(effort) = self.reasoning_effort {
            args.push("--thinking".to_string());
            args.push(effort.to_string());
        }
        args
    }

    /// Pi merges piped stdin into the initial prompt in every non-interactive
    /// mode. Keeping the execution envelope off argv prevents task context from
    /// entering process listings, audit argv, or spawn diagnostics.
    pub(crate) fn stdin(&self, envelope_json: &[u8]) -> Vec<u8> {
        render_prompt_with_embedded_envelope(envelope_json)
    }

    pub(crate) fn model_name(&self) -> Option<&str> {
        self.model.as_deref()
    }
}
