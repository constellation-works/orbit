use orbit_common::OrbitError;
use orbit_types::telemetry::InvocationTrace;
use orbit_types::tool::ExecutionResult;
use orbit_types::workflow::{AgentResponseEnvelope, AgentRunError};
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
        Err(err) if is_discovery_limit_error(&err) => Err(err),
        Err(err) => synthesize_response(exec_result).ok_or(err),
    }
}

pub fn is_timeout(exec_result: &ExecutionResult) -> bool {
    exec_result.timed_out
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
/// Returns `None` when stdout cannot be parsed, carries no recognizable
/// envelope, or discovery exhausts its work bound. Validating APIs fail that
/// last case closed instead of treating it as absent.
pub fn peek_response_status(stdout: &str) -> Option<String> {
    let documents = parse_json_documents(stdout).ok()?;
    match discover_in_values(
        documents.iter().rev(),
        &mut Budget::production(),
        deserialize_envelope,
    ) {
        Ok(Some(envelope)) => Some(envelope.status),
        Ok(None) | Err(_) => None,
    }
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
    discover_in_values(
        documents.iter().rev(),
        &mut Budget::production(),
        declared_response_failure,
    )
    .unwrap_or_default()
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
        Ok(documents) => discover_in_values(
            documents.iter().rev(),
            &mut Budget::production(),
            deserialize_envelope,
        )?,
        Err(_) => discover_in_string(stdout, &mut Budget::production(), deserialize_envelope)?,
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
    let envelope = discover_in_values(
        documents.iter().rev(),
        &mut Budget::production(),
        deserialize_envelope,
    )?
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
    match discover_in_values(
        documents.iter(),
        &mut Budget::production(),
        deserialize_envelope,
    ) {
        Ok(None) => {}
        Ok(Some(_)) | Err(_) => return None,
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

/// Suffix JSON parses allowed while searching: the whole-string attempt plus
/// each `{` candidate that is actually deserialized.
pub(in crate::types) const ENVELOPE_DISCOVERY_MAX_PARSE_ATTEMPTS: u32 = 4_096;

/// JSON nodes visited while searching for an envelope or declared failure.
pub(in crate::types) const ENVELOPE_DISCOVERY_MAX_NODES: u32 = 65_536;

const DISCOVERY_LIMIT_PREFIX: &str = "response envelope discovery exceeded a work limit";

// [ORB-10746] `structured_output` first: with `--json-schema` in play it is
// the schema-validated object the provider committed to, and Claude duplicates
// it into `result` only as a JSON-encoded string. `result` is null on several
// terminal paths where `structured_output` still holds the envelope, so probing
// it first is the difference between reading the authoritative field and
// parsing a copy by luck.
const PREFERRED_OBJECT_KEYS: [&str; 9] = [
    "structured_output",
    "result",
    "response",
    "message",
    "messages",
    "content",
    "final",
    "final_message",
    "output",
];

/// Caps for one envelope or declared-failure search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::types) struct EnvelopeDiscoveryBudget {
    pub max_parse_attempts: u32,
    pub max_nodes: u32,
}

impl EnvelopeDiscoveryBudget {
    pub const fn production() -> Self {
        Self {
            max_parse_attempts: ENVELOPE_DISCOVERY_MAX_PARSE_ATTEMPTS,
            max_nodes: ENVELOPE_DISCOVERY_MAX_NODES,
        }
    }

    #[cfg(test)]
    pub const fn new(max_parse_attempts: u32, max_nodes: u32) -> Self {
        Self {
            max_parse_attempts,
            max_nodes,
        }
    }
}

/// Parse/traversal accounting for one search. Sibling tests use this to pin
/// the documented work bound and the skip-already-visited preferred-key rule.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(in crate::types) struct EnvelopeDiscoveryStats {
    pub parse_attempts: u32,
    pub nodes_visited: u32,
}

struct Budget {
    max_parse_attempts: u32,
    max_nodes: u32,
    stats: EnvelopeDiscoveryStats,
}

impl Budget {
    fn new(limits: EnvelopeDiscoveryBudget) -> Self {
        Self {
            max_parse_attempts: limits.max_parse_attempts,
            max_nodes: limits.max_nodes,
            stats: EnvelopeDiscoveryStats::default(),
        }
    }

    fn production() -> Self {
        Self::new(EnvelopeDiscoveryBudget::production())
    }

    fn visit(&mut self) -> Result<(), OrbitError> {
        self.stats.nodes_visited = self.stats.nodes_visited.saturating_add(1);
        if self.stats.nodes_visited > self.max_nodes {
            Err(discovery_limit_error(
                "traversed nodes",
                self.stats.nodes_visited,
                self.max_nodes,
            ))
        } else {
            Ok(())
        }
    }

    fn parse(&mut self) -> Result<(), OrbitError> {
        self.stats.parse_attempts = self.stats.parse_attempts.saturating_add(1);
        if self.stats.parse_attempts > self.max_parse_attempts {
            Err(discovery_limit_error(
                "parse attempts",
                self.stats.parse_attempts,
                self.max_parse_attempts,
            ))
        } else {
            Ok(())
        }
    }
}

fn discovery_limit_error(what: &str, actual: u32, max: u32) -> OrbitError {
    OrbitError::AgentProtocolViolation(format!(
        "{DISCOVERY_LIMIT_PREFIX}: {what} {actual} > max {max}"
    ))
}

fn is_discovery_limit_error(err: &OrbitError) -> bool {
    match err {
        OrbitError::AgentProtocolViolation(message) => message.starts_with(DISCOVERY_LIMIT_PREFIX),
        _ => false,
    }
}

/// Protocol-check search: JSON documents when the stream parses, otherwise a
/// bounded suffix scan of mixed stdout. Visible to sibling tests.
#[cfg(test)]
pub(in crate::types) fn discover_agent_response_envelope_with_stats(
    stdout: &str,
    budget: EnvelopeDiscoveryBudget,
) -> Result<(Option<AgentResponseEnvelope>, EnvelopeDiscoveryStats), OrbitError> {
    let mut budget = Budget::new(budget);
    let found = match parse_json_documents(stdout) {
        Ok(documents) => {
            discover_in_values(documents.iter().rev(), &mut budget, deserialize_envelope)?
        }
        Err(_) => discover_in_string(stdout, &mut budget, deserialize_envelope)?,
    };
    Ok((found, budget.stats))
}

/// Declared-failure search with the same walk and bounds as envelope
/// discovery, including the mixed-stdout suffix scan used inside string
/// fields. Visible to sibling tests.
#[cfg(test)]
pub(in crate::types) fn discover_declared_response_failure_with_stats(
    stdout: &str,
    budget: EnvelopeDiscoveryBudget,
) -> Result<(Option<DeclaredResponseFailure>, EnvelopeDiscoveryStats), OrbitError> {
    let mut budget = Budget::new(budget);
    let found = match parse_json_documents(stdout) {
        Ok(documents) => discover_in_values(
            documents.iter().rev(),
            &mut budget,
            declared_response_failure,
        )?,
        Err(_) => discover_in_string(stdout, &mut budget, declared_response_failure)?,
    };
    Ok((found, budget.stats))
}

fn discover_in_values<'a, T, I, F>(
    values: I,
    budget: &mut Budget,
    inspect: F,
) -> Result<Option<T>, OrbitError>
where
    I: IntoIterator<Item = &'a Value>,
    F: Fn(&Value) -> Option<T>,
{
    for value in values {
        if let Some(found) = walk(value, budget, &inspect)? {
            return Ok(Some(found));
        }
    }
    Ok(None)
}

fn discover_in_string<T, F>(
    raw: &str,
    budget: &mut Budget,
    inspect: F,
) -> Result<Option<T>, OrbitError>
where
    F: Fn(&Value) -> Option<T>,
{
    walk_string(raw, budget, &inspect)
}

fn walk<T, F>(value: &Value, budget: &mut Budget, inspect: &F) -> Result<Option<T>, OrbitError>
where
    F: Fn(&Value) -> Option<T>,
{
    budget.visit()?;
    if let Some(found) = inspect(value) {
        return Ok(Some(found));
    }
    match value {
        Value::String(raw) => walk_string(raw, budget, inspect),
        Value::Array(items) => {
            for item in items.iter().rev() {
                if let Some(found) = walk(item, budget, inspect)? {
                    return Ok(Some(found));
                }
            }
            Ok(None)
        }
        Value::Object(map) => walk_object(map, budget, inspect),
        _ => Ok(None),
    }
}

fn walk_object<T, F>(
    map: &serde_json::Map<String, Value>,
    budget: &mut Budget,
    inspect: &F,
) -> Result<Option<T>, OrbitError>
where
    F: Fn(&Value) -> Option<T>,
{
    for key in PREFERRED_OBJECT_KEYS {
        if let Some(child) = map.get(key)
            && let Some(found) = walk(child, budget, inspect)?
        {
            return Ok(Some(found));
        }
    }

    for (key, child) in map {
        if PREFERRED_OBJECT_KEYS.contains(&key.as_str()) {
            continue;
        }
        if let Some(found) = walk(child, budget, inspect)? {
            return Ok(Some(found));
        }
    }
    Ok(None)
}

fn walk_string<T, F>(raw: &str, budget: &mut Budget, inspect: &F) -> Result<Option<T>, OrbitError>
where
    F: Fn(&Value) -> Option<T>,
{
    budget.parse()?;
    if let Ok(nested) = serde_json::from_str::<Value>(raw) {
        return walk(&nested, budget, inspect);
    }

    let mut search_from = 0;
    while let Some(rel) = raw[search_from..].find('{') {
        let start = search_from + rel;
        budget.parse()?;
        let mut stream = Deserializer::from_str(&raw[start..]).into_iter::<Value>();
        match stream.next() {
            Some(Ok(nested)) => {
                let consumed = stream.byte_offset().max(1);
                if let Some(found) = walk(&nested, budget, inspect)? {
                    return Ok(Some(found));
                }
                search_from = start + consumed;
            }
            Some(Err(_)) | None => {
                search_from = start + 1;
            }
        }
    }
    Ok(None)
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
