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

/// [ORB-13664] Headless `-p --json-schema` mode forces the StructuredOutput
/// call as soon as the agent ends its turn, so a validation gate started with
/// Bash `run_in_background` and awaited through `Monitor` is still running when
/// the envelope is written (`validation_incomplete` in `jrun-20260928-0259-c1`
/// and `jrun-20260928-0305-c1`). This switch removes the `run_in_background`
/// parameter and the `Monitor` tool, so every gate runs in the foreground.
/// It is an env var rather than a static arg for the same reason
/// `--json-schema` is emitted here: the installed `claude.yaml` copy is edited
/// independently of the packaged asset.
///
/// [ORB-14815] `CLAUDE_CODE_DISABLE_CRON` (Claude Code 2.1.294, checked in the
/// installed binary) disables the cron and loop scheduler, so a queued
/// `ScheduleWakeup` never fires. Without it a worker that armed a loop while it
/// waited on gates got a second `result` turn of prose after the envelope turn
/// (`jrun-20261008-1237-c25`, `jrun-20261008-1421-c5`).
pub(crate) const CLAUDE_CLI_FIXED_ENV: &[(&str, &str)] = &[
    ("CLAUDE_CODE_DISABLE_BACKGROUND_TASKS", "1"),
    ("CLAUDE_CODE_DISABLE_CRON", "1"),
];

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
    // adds per-request toggles and the flags Orbit's response contract needs.
    pub(crate) fn args(&self) -> Vec<String> {
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
        //
        // [ORB-14696] Claude reports its usage windows only as
        // `rate_limit_event` messages, which `--output-format json` drops.
        // `stream-json` (which needs `--verbose`) keeps them, one JSONL frame
        // each. It overrides the executor's static `--output-format json`, as
        // the later flag wins, so the installed `claude.yaml` copy needs no
        // edit. Not `json --verbose`: that prints the whole session as one
        // line, which a run longer than the 1 MiB stdout capture loses, and
        // with it the terminal `result`. Completion reads only that terminal
        // `result` (`project_claude_response`).
        let mut args = vec![
            "--json-schema".to_string(),
            response_envelope_json_schema_arg(),
            "--output-format".to_string(),
            "stream-json".to_string(),
            "--verbose".to_string(),
        ];

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
