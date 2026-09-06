use orbit_common::OrbitError;
use orbit_types::telemetry::InvocationTrace;
use orbit_types::tool::ExecutionResult;
use orbit_types::workflow::{AgentResponseEnvelope, AgentRunError};
use serde::Deserialize;
use serde_json::{Deserializer, Value};

use super::protocol_schema::{RESPONSE_ENVELOPE_SCHEMA_VERSION, RESPONSE_ENVELOPE_STATUSES};
use super::wrapper::wrapper_signals;
use super::{AgentResponseStatus, ResponseParseResult, trace::extract_invocation_trace};

#[derive(Debug, Clone, PartialEq)]
pub struct DeclaredResponseFailure {
    pub status: String,
    pub error: Option<AgentRunError>,
}

pub fn parse_and_validate_response(exec_result: &ExecutionResult) -> ResponseParseResult {
    match parse_json_envelope(exec_result) {
        Ok(parsed) => Ok(parsed),
        Err(err) => synthesize_response(exec_result).ok_or(err),
    }
}

pub fn is_timeout(exec_result: &ExecutionResult) -> bool {
    !exec_result.success && exec_result.stderr.contains("process timed out")
}

/// Best-effort lookup of an embedded Orbit response envelope's `status` field
/// in raw subprocess stdout, *without* validating exit-code alignment.
///
/// Used by the CLI dispatcher (T20260508-17) to demote `success` when a CLI
/// like Claude exits 0 with a wrapping `result.subtype = "success"` but its
/// embedded Orbit envelope reports `status = "failed"`. `parse_and_validate_response`
/// returns `Err` in that case because exit alignment fails, which threw away
/// the signal the dispatcher needs to classify the outcome.
///
/// Returns `None` when stdout cannot be parsed or carries no recognizable
/// envelope, so callers can fall through to other classification rather than
/// regressing legacy provider shapes.
pub fn peek_response_status(stdout: &str) -> Option<String> {
    let documents = parse_json_documents(stdout).ok()?;
    let envelope = documents
        .iter()
        .rev()
        .find_map(find_agent_response_envelope)?;
    Some(envelope.status)
}

/// Best-effort lookup of a terminal failure declaration in provider stdout.
///
/// Unlike full response validation, this preserves the status when its error
/// object is absent or malformed. The dispatcher uses that status to fail
/// closed, while treating unavailable error details as a generic diagnostic.
/// The returned error is present only when both its code and message are
/// non-empty strings.
pub fn peek_declared_response_failure(stdout: &str) -> Option<DeclaredResponseFailure> {
    let documents = parse_json_documents(stdout).ok()?;
    documents
        .iter()
        .rev()
        .find_map(find_declared_response_failure)
}

/// Content-blind check that a provider's stdout *terminated with* a well-formed
/// Orbit response envelope.
///
/// This is the step-completion protocol signal ([ADR-0258] / [ORB-10449]), not a
/// judgement about the work: it reads only the envelope frame — that stdout
/// parsed as JSON, that a recognizable envelope is present, that its
/// `schemaVersion` is supported, and that its `status` is one of the three
/// protocol tokens. It never reads `result` or `error`, and it does not care
/// *which* status was declared. An agent that reports `status: "failed"`
/// completed its invocation contract exactly as much as one that reports
/// success; an agent that yielded mid-turn emitted no envelope at all.
///
/// Deliberately excludes [`validate_exit_alignment`]. Exit agreement is a
/// separate classification the CLI runner already performs, and folding it in
/// here would make a `status: "failed"` envelope indistinguishable from a
/// missing one — which is precisely the distinction this predicate exists to
/// draw.
pub fn response_envelope_protocol_check(stdout: &str) -> Result<(), OrbitError> {
    // A provider may interleave non-protocol chatter with its output — a
    // wrapped tool writing to the same stdout, a warning line — which makes the
    // stream unparseable as a whole even though the agent did terminate
    // properly. Fall back to scanning the raw text so this gate tests for the
    // termination signal, not for the tidiness of the stream around it. Failing
    // a completed step over stray stdout would be a worse defect than the one
    // this check exists to catch.
    let envelope = match parse_json_documents(stdout) {
        Ok(documents) => documents
            .iter()
            .rev()
            .find_map(find_agent_response_envelope),
        Err(_) => find_agent_response_envelope_in_string(stdout),
    };
    let envelope = envelope
        .ok_or_else(|| OrbitError::AgentProtocolViolation(missing_envelope_message(stdout)))?;
    if envelope.schema_version != RESPONSE_ENVELOPE_SCHEMA_VERSION {
        return Err(OrbitError::AgentProtocolViolation(format!(
            "unsupported schemaVersion: {}",
            envelope.schema_version
        )));
    }
    if !RESPONSE_ENVELOPE_STATUSES.contains(&envelope.status.as_str()) {
        return Err(OrbitError::AgentProtocolViolation(format!(
            "unknown status: {}",
            envelope.status
        )));
    }
    Ok(())
}

/// The invariant the [ORB-10449] completion guard is built on. Kept verbatim
/// as the prefix of every missing-envelope diagnostic so the failure stays
/// recognizable while [ORB-10746] appends a cause when one is available.
const MISSING_ENVELOPE_MESSAGE: &str = "stdout does not contain an Orbit response envelope";

/// [ORB-10746] Explain *why* a run ended without an envelope when the
/// provider's own wrapper says the ending was abnormal.
///
/// A turn-limit or otherwise-errored ending exits 0 with no envelope, exactly
/// like an agent that answered in prose, and used to be indistinguishable from
/// it. This changes only the message: the decision to fail was already made by
/// the caller, and no wrapper signal can reverse it.
fn missing_envelope_message(stdout: &str) -> String {
    let Some(diagnostic) = parse_json_documents(stdout)
        .ok()
        .and_then(|documents| wrapper_signals(&documents).terminal_ending_diagnostic())
    else {
        return MISSING_ENVELOPE_MESSAGE.to_string();
    };
    format!("{MISSING_ENVELOPE_MESSAGE}: {diagnostic}")
}

fn parse_json_documents(stdout: &str) -> Result<Vec<Value>, OrbitError> {
    let mut documents = Vec::new();
    for item in Deserializer::from_str(stdout).into_iter::<Value>() {
        let value = item.map_err(|error| {
            OrbitError::AgentProtocolViolation(format!("stdout is not valid JSON: {error}"))
        })?;
        documents.push(value);
    }
    if documents.is_empty() {
        return Err(OrbitError::AgentProtocolViolation(
            "stdout does not contain a JSON document".to_string(),
        ));
    }
    Ok(documents)
}

fn validate_exit_alignment(
    exec_result: &ExecutionResult,
    envelope: &AgentResponseEnvelope,
) -> Result<(), OrbitError> {
    let timed_out = is_timeout(exec_result);

    if timed_out && envelope.status != "timeout" {
        return Err(OrbitError::AgentProtocolViolation(
            "timeout process must report status=timeout".to_string(),
        ));
    }

    if timed_out {
        return Ok(());
    }

    let exit_code = exec_result.exit_code.unwrap_or(1);
    if exit_code == 0 && envelope.status != "success" {
        return Err(OrbitError::AgentProtocolViolation(
            "exit_code=0 must report status=success".to_string(),
        ));
    }
    if exit_code != 0 && envelope.status == "success" {
        return Err(OrbitError::AgentProtocolViolation(
            "non-zero exit code cannot report status=success".to_string(),
        ));
    }

    Ok(())
}

fn parse_json_envelope(exec_result: &ExecutionResult) -> ResponseParseResult {
    let documents = parse_json_documents(&exec_result.stdout)?;
    let envelope = documents
        .iter()
        .rev()
        .find_map(find_agent_response_envelope)
        .ok_or_else(|| {
            OrbitError::AgentProtocolViolation(missing_envelope_message(&exec_result.stdout))
        })?;
    let trace = extract_invocation_trace(&documents, exec_result.duration_ms);

    if envelope.schema_version != RESPONSE_ENVELOPE_SCHEMA_VERSION {
        return Err(OrbitError::AgentProtocolViolation(format!(
            "unsupported schemaVersion: {}",
            envelope.schema_version
        )));
    }

    let state = match envelope.status.as_str() {
        "success" => AgentResponseStatus::Success,
        "failed" => {
            let Some(error) = &envelope.error else {
                return Err(OrbitError::AgentProtocolViolation(
                    "failed status requires error object".to_string(),
                ));
            };
            if error.code.trim().is_empty() {
                return Err(OrbitError::AgentProtocolViolation(
                    "failed status requires non-empty error.code".to_string(),
                ));
            }
            AgentResponseStatus::Failed
        }
        "timeout" => AgentResponseStatus::Timeout,
        other => {
            return Err(OrbitError::AgentProtocolViolation(format!(
                "unknown status: {other}"
            )));
        }
    };

    validate_exit_alignment(exec_result, &envelope)?;
    Ok((envelope, state, trace))
}

// Visible through `response.rs` to sibling-layout tests; keeping this private
// would require nesting tests back under `envelope`.
pub(in crate::types) fn synthesize_response(
    exec_result: &ExecutionResult,
) -> Option<(AgentResponseEnvelope, AgentResponseStatus, InvocationTrace)> {
    if is_timeout(exec_result) {
        return Some((
            AgentResponseEnvelope {
                schema_version: 1,
                status: "timeout".to_string(),
                result: None,
                error: Some(AgentRunError {
                    code: "AGENT_TIMEOUT".to_string(),
                    message: "agent timed out".to_string(),
                    details: Value::Null,
                }),
                duration_ms: Some(exec_result.duration_ms),
            },
            AgentResponseStatus::Timeout,
            synthesize_trace(exec_result),
        ));
    }

    // [ORB-10746] An exit-0 ending the provider itself flagged as abnormal —
    // a turn limit, an errored terminal reason — carries no envelope, so the
    // guard above would have returned `None` and the run would surface only
    // the generic missing-envelope text. Synthesize the failure it plainly is,
    // with the provider's own reason attached.
    //
    // This is the only new synthesis path, and it is failure-only by
    // construction: no combination of `is_error`, `subtype`, `terminal_reason`,
    // exit code, or provider prose can produce a `success` envelope here.
    if let Some(diagnostic) = exit_zero_terminal_failure(exec_result) {
        return Some(synthesized_failure(
            exec_result,
            "AGENT_TERMINAL_ENDING",
            diagnostic,
        ));
    }

    if exec_result.exit_code.unwrap_or(1) == 0 || !exec_result.stdout.trim().is_empty() {
        return None;
    }

    Some(synthesized_failure(
        exec_result,
        "AGENT_INVOCATION_FAILED",
        synthetic_error_message(exec_result),
    ))
}

fn synthesized_failure(
    exec_result: &ExecutionResult,
    code: &str,
    message: String,
) -> (AgentResponseEnvelope, AgentResponseStatus, InvocationTrace) {
    (
        AgentResponseEnvelope {
            schema_version: RESPONSE_ENVELOPE_SCHEMA_VERSION,
            status: "failed".to_string(),
            result: None,
            error: Some(AgentRunError {
                code: code.to_string(),
                message,
                details: Value::Null,
            }),
            duration_ms: Some(exec_result.duration_ms),
        },
        AgentResponseStatus::Failed,
        synthesize_trace(exec_result),
    )
}

/// A clean exit whose wrapper nonetheless reports an abnormal ending, with no
/// envelope anywhere in stdout.
///
/// Requires the absence of an envelope: a real envelope is the authoritative
/// outcome whatever its status, and must never be displaced by a synthesized
/// one — that would let wrapper prose overrule the protocol.
fn exit_zero_terminal_failure(exec_result: &ExecutionResult) -> Option<String> {
    if exec_result.exit_code.unwrap_or(1) != 0 {
        return None;
    }
    let documents = parse_json_documents(&exec_result.stdout).ok()?;
    if documents
        .iter()
        .any(|document| find_agent_response_envelope(document).is_some())
    {
        return None;
    }
    wrapper_signals(&documents).terminal_ending_diagnostic()
}

// Best-effort trace extraction for the fallback path. Provider CLIs (e.g.
// `claude -p --output-format json`) emit a wrapping JSON document whose
// `usage` block carries token counts even when the embedded Orbit response
// envelope is malformed or missing — losing that data on the synthesize path
// is what made claude show as zero tokens on the scoreboard.
// Visible through `response.rs` to sibling-layout tests; this is a narrow
// crate-internal seam for fallback trace behavior.
pub(in crate::types) fn synthesize_trace(exec_result: &ExecutionResult) -> InvocationTrace {
    match parse_json_documents(&exec_result.stdout) {
        Ok(documents) => extract_invocation_trace(&documents, exec_result.duration_ms),
        Err(_) => InvocationTrace {
            duration_ms: exec_result.duration_ms,
            ..InvocationTrace::default()
        },
    }
}

fn synthetic_error_message(exec_result: &ExecutionResult) -> String {
    let stderr = exec_result.stderr.trim();
    if !stderr.is_empty() {
        return stderr.to_string();
    }
    let stdout = exec_result.stdout.trim();
    if !stdout.is_empty() {
        return stdout.to_string();
    }
    "agent execution failed".to_string()
}

fn find_agent_response_envelope(value: &Value) -> Option<AgentResponseEnvelope> {
    if let Some(envelope) = deserialize_envelope(value) {
        return Some(envelope);
    }

    match value {
        Value::String(raw) => find_agent_response_envelope_in_string(raw),
        Value::Array(items) => items.iter().rev().find_map(find_agent_response_envelope),
        Value::Object(map) => {
            for key in [
                // [ORB-10746] `structured_output` first: with `--json-schema`
                // in play it is the schema-validated object the provider
                // committed to, and Claude duplicates it into `result` only as
                // a JSON-encoded string. `result` is null on several terminal
                // paths where `structured_output` still holds the envelope, so
                // probing it first is the difference between reading the
                // authoritative field and parsing a copy by luck.
                "structured_output",
                "result",
                "response",
                "message",
                "messages",
                "content",
                "final",
                "final_message",
                "output",
            ] {
                if let Some(found) = map.get(key).and_then(find_agent_response_envelope) {
                    return Some(found);
                }
            }

            map.values().find_map(find_agent_response_envelope)
        }
        _ => None,
    }
}

fn find_declared_response_failure(value: &Value) -> Option<DeclaredResponseFailure> {
    if let Some(failure) = declared_response_failure(value) {
        return Some(failure);
    }

    match value {
        Value::String(raw) => find_declared_response_failure_in_string(raw),
        Value::Array(items) => items.iter().rev().find_map(find_declared_response_failure),
        Value::Object(map) => {
            for key in [
                "structured_output",
                "result",
                "response",
                "message",
                "messages",
                "content",
                "final",
                "final_message",
                "output",
            ] {
                if let Some(found) = map.get(key).and_then(find_declared_response_failure) {
                    return Some(found);
                }
            }

            map.values().find_map(find_declared_response_failure)
        }
        _ => None,
    }
}

fn find_declared_response_failure_in_string(raw: &str) -> Option<DeclaredResponseFailure> {
    if let Ok(nested) = serde_json::from_str::<Value>(raw)
        && let Some(failure) = find_declared_response_failure(&nested)
    {
        return Some(failure);
    }

    raw.match_indices('{').find_map(|(start, _)| {
        let mut deserializer = Deserializer::from_str(&raw[start..]);
        let nested = Value::deserialize(&mut deserializer).ok()?;
        find_declared_response_failure(&nested)
    })
}

fn declared_response_failure(value: &Value) -> Option<DeclaredResponseFailure> {
    let object = value.as_object()?;
    let schema_version = object.get("schemaVersion")?.as_u64()?;
    if schema_version != RESPONSE_ENVELOPE_SCHEMA_VERSION as u64 {
        return None;
    }

    let status = object.get("status")?.as_str()?;
    if !matches!(status, "failed" | "timeout") {
        return None;
    }

    let error = object
        .get("error")
        .and_then(Value::as_object)
        .and_then(|error| {
            let code = error.get("code")?.as_str()?.trim();
            let message = error.get("message")?.as_str()?.trim();
            (!code.is_empty() && !message.is_empty()).then(|| AgentRunError {
                code: code.to_string(),
                message: message.to_string(),
                details: Value::Null,
            })
        });

    Some(DeclaredResponseFailure {
        status: status.to_string(),
        error,
    })
}

fn find_agent_response_envelope_in_string(raw: &str) -> Option<AgentResponseEnvelope> {
    if let Ok(nested) = serde_json::from_str::<Value>(raw)
        && let Some(envelope) = find_agent_response_envelope(&nested)
    {
        return Some(envelope);
    }

    raw.match_indices('{').find_map(|(start, _)| {
        let mut deserializer = Deserializer::from_str(&raw[start..]);
        let nested = Value::deserialize(&mut deserializer).ok()?;
        find_agent_response_envelope(&nested)
    })
}

fn deserialize_envelope(value: &Value) -> Option<AgentResponseEnvelope> {
    let object = value.as_object()?;
    if !object.contains_key("schemaVersion") || !object.contains_key("status") {
        return None;
    }
    // [ORB-10770] Claude's constrained decoder, given an untyped `error` and a
    // description that said "null when status is success", emits the JSON
    // *string* `"null"` (`jrun-20260813-0451-3`). `Option<AgentRunError>`
    // cannot accept a string, so the finder used to miss a completed
    // envelope. Treat `"null"` / `""` as absent; JSON null is already `None`
    // via `#[serde(default)]`. This only makes a recognizable frame parse —
    // it does not infer success from wrapper signals.
    match object.get("error") {
        Some(Value::String(token)) if is_absent_error_string(token) => {
            let mut object = object.clone();
            object.remove("error");
            serde_json::from_value(Value::Object(object)).ok()
        }
        _ => serde_json::from_value(value.clone()).ok(),
    }
}

fn is_absent_error_string(token: &str) -> bool {
    let trimmed = token.trim();
    trimmed.is_empty() || trimmed == "null"
}
