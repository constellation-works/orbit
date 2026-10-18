use orbit_types::identity::ReasoningEffort;

use crate::providers::common::render_prompt_with_embedded_envelope;

/// The `muse exec` flag that names a prompt file. [ORB-16577]
///
/// Muse Code 1.4.10 cannot take the prompt on stdin: piped stdin without a
/// positional prompt exits 2 with "missing prompt", and
/// `--prompt-file /dev/stdin` is refused as "not a regular file". The runner
/// therefore writes the prompt to a private regular file and passes its path
/// here, which keeps the execution envelope off argv.
pub(crate) const MUSE_PROMPT_FILE_FLAG: &str = "--prompt-file";

/// Per-request command construction for the Muse Code CLI. [ORB-16577]
///
/// The shipped executor owns the static headless flags (`exec`, `--json`,
/// approval, reminder, session-log and personal-context switches); this
/// transport adds only what Orbit's crew resolution decides per invocation.
pub(crate) struct MuseCliTransport {
    model: Option<String>,
    reasoning_effort: Option<ReasoningEffort>,
}

impl MuseCliTransport {
    pub(crate) fn new(model: Option<String>, reasoning_effort: Option<ReasoningEffort>) -> Self {
        Self {
            model,
            reasoning_effort,
        }
    }

    /// `--model` takes a Muse catalog id and never changes the Orbit executor
    /// identity. `--reasoning-effort` carries the crew effort verbatim: every
    /// Orbit effort is a Muse tier, so nothing is remapped.
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

    /// The prompt bytes, which the runner delivers through the prompt file.
    pub(crate) fn prompt(&self, envelope_json: &[u8]) -> Vec<u8> {
        render_prompt_with_embedded_envelope(envelope_json)
    }

    pub(crate) fn model_name(&self) -> Option<&str> {
        self.model.as_deref()
    }
}
