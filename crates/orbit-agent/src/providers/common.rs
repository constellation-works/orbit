use tracing::debug;

// The CLI providers share this single response-frame rendering path. Activity
// instructions describe only their result fields and durable side effects.
const ORBIT_RESPONSE_CONTRACT: &str = "You are Orbit's agent executor.\n\
Read the execution envelope JSON and perform the requested work.\n\
Return exactly one JSON object and nothing else.\n\
Required response schema:\n\
{\"schemaVersion\":1,\"status\":\"success|failed|timeout\",\"result\":{...},\"error\":null}\n\
Rules:\n\
- Output valid JSON only. No markdown fences. No explanatory text.\n\
- result MUST be a JSON object (never null, never omitted). It may be {} for side-effect-only activities.\n\
- If execution cannot complete, return status=\"failed\" with non-empty error.code and error.message (result may be {}).\n\
- Persist meaningful state via task artifacts (orbit.task.update), not via the result object.\n\
- This is one non-interactive session and it ends when you stop. Run every command in the foreground and wait for its result, with a long enough timeout. Never leave a process running past the command that started it (nohup, a trailing &, setsid, disown), and never plan to check on one later: nothing resumes you, and its result is lost.\n\
- This host is shared with other runs. Never generate synthetic load (CPU burners, busy or stress loops, parallel hammering of tests) to reproduce a flake or race. Reproduce the timing deterministically in a harness, or report the finding as unconfirmed.";

pub(crate) fn render_prompt_with_embedded_envelope(envelope_json: &[u8]) -> Vec<u8> {
    debug!(
        envelope_bytes = envelope_json.len(),
        "constructed embedded Orbit execution prompt"
    );
    let envelope_text = String::from_utf8_lossy(envelope_json);
    format!("{ORBIT_RESPONSE_CONTRACT}\nExecution envelope:\n{envelope_text}\n").into_bytes()
}
