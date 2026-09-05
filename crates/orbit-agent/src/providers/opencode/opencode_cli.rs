use orbit_types::identity::ReasoningEffort;

use crate::providers::common::render_prompt_with_embedded_envelope;

/// Per-request command construction for the OpenCode CLI. [ORB-11295]
///
/// The shipped executor owns the static headless flags (`run`, `--format json`,
/// `--auto`); this transport adds only what Orbit's crew resolution decides per
/// invocation.
pub(crate) struct OpencodeCliTransport {
    model: Option<String>,
    reasoning_effort: Option<ReasoningEffort>,
}

impl OpencodeCliTransport {
    pub(crate) fn new(model: Option<String>, reasoning_effort: Option<ReasoningEffort>) -> Self {
        Self {
            model,
            reasoning_effort,
        }
    }

    /// `--model` takes OpenCode's `provider/model` coordinate, which names the
    /// *model vendor* inside the OpenCode lane and never changes the Orbit
    /// executor identity. `--variant` carries the crew effort; the agent
    /// configuration boundary has already rejected any effort outside
    /// OpenCode's documented variant vocabulary, so nothing is remapped or
    /// silently dropped here.
    pub(crate) fn args(&self) -> Vec<String> {
        let mut args = Vec::new();
        if let Some(model) = &self.model {
            args.push("--model".to_string());
            args.push(model.clone());
        }
        if let Some(effort) = self.reasoning_effort {
            args.push("--variant".to_string());
            args.push(effort.to_string());
        }
        args
    }

    /// OpenCode reads piped stdin whenever stdin is not a TTY and uses it as
    /// the whole message when no positional `[message..]` is supplied, so the
    /// execution envelope never reaches argv — and therefore never reaches
    /// process listings, audit argv, or spawn diagnostics.
    pub(crate) fn stdin(&self, envelope_json: &[u8]) -> Vec<u8> {
        render_prompt_with_embedded_envelope(envelope_json)
    }

    pub(crate) fn model_name(&self) -> Option<&str> {
        self.model.as_deref()
    }
}
