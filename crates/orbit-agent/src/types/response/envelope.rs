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

/// JSON documents from one stdout capture.
///
/// CLI post-run handling peeks status, checks the completion envelope, and
/// extracts a result/trace from the same capture. Parsing JSONL once avoids
/// materialising `Vec<Value>` for each helper.
#[derive(Debug)]
pub struct ParsedStdout<'a> {
    raw: &'a str,
    documents: Result<Vec<Value>, String>,
}

impl<'a> ParsedStdout<'a> {
    pub fn parse(stdout: &'a str) -> Self {
        Self {
            raw: stdout,
            documents: parse_json_documents(stdout).map_err(protocol_violation_message),
        }
    }

    pub fn raw(&self) -> &'a str {
        self.raw
    }

    fn json_documents(&self) -> Result<&[Value], OrbitError> {
        self.documents
            .as_ref()
            .map(Vec::as_slice)
            .map_err(|message| OrbitError::AgentProtocolViolation(message.clone()))
    }

    pub fn peek_response_status(&self) -> Option<String> {
        self.discover(response_candidate)
            .unwrap_or_default()
            .and_then(|candidate| candidate.get("status")?.as_str().map(str::to_owned))
    }

    pub fn peek_declared_response_failure(&self) -> Option<DeclaredResponseFailure> {
        self.discover(response_candidate)
            .unwrap_or_default()
            .and_then(|candidate| declared_response_failure(&candidate))
    }

    fn discover<T>(&self, inspect: impl Fn(&Value) -> Option<T>) -> Result<Option<T>, OrbitError> {
        // A provider may interleave non-protocol chatter with its output — a
        // wrapped tool writing to the same stdout, a warning line — which makes
        // the stream unparseable as a whole even though the agent did terminate
        // properly. Fall back to scanning the raw text so this gate tests for
        // the termination signal, not for the tidiness of the stream around it.
        // Response parsing and status peeking use this same bounded search:
        // only a complete JSON object supplies fields, never surrounding prose.
        // Documents and encoded/prose answers all select newest first.
        match self.json_documents() {
            Ok(documents) => {
                discover_in_values(documents.iter().rev(), &mut Budget::production(), &inspect)
            }
            Err(_) => discover_in_string(self.raw, &mut Budget::production(), inspect),
        }
    }

    pub fn response_envelope_protocol_check(&self) -> Result<(), OrbitError> {
        validate_envelope_frame(&self.response_candidate()?)
    }

    fn response_candidate(&self) -> Result<Value, OrbitError> {
        self.discover(response_candidate)?
            .ok_or_else(|| OrbitError::AgentProtocolViolation(missing_envelope_message(self)))
    }

    pub fn parse_and_validate(&self, exec_result: &ExecutionResult) -> ResponseParseResult {
        match parse_json_envelope(exec_result, self) {
            Ok(parsed) => Ok(parsed),
            Err(err) if is_discovery_limit_error(&err) => Err(err),
            Err(err) => synthesize_from_parsed(exec_result, self).ok_or(err),
        }
    }
}

fn protocol_violation_message(error: OrbitError) -> String {
    match error {
        OrbitError::AgentProtocolViolation(message) => message,
        other => other.to_string(),
    }
}

#[cfg(test)]
pub fn parse_and_validate_response(exec_result: &ExecutionResult) -> ResponseParseResult {
    ParsedStdout::parse(&exec_result.stdout).parse_and_validate(exec_result)
}

pub fn is_timeout(exec_result: &ExecutionResult) -> bool {
    exec_result.timed_out
}

/// Content-blind check that a provider's stdout *terminated with* a well-formed
/// Orbit response envelope.
///
/// This is the step-completion protocol signal ([ADR-0258] / [ORB-10449]), not a
/// judgement about the work: it reads only the envelope frame — that a
/// recognizable complete JSON envelope is present in stdout, that its
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
#[cfg(test)]
pub fn response_envelope_protocol_check(stdout: &str) -> Result<(), OrbitError> {
    ParsedStdout::parse(stdout).response_envelope_protocol_check()
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
fn missing_envelope_message(parsed: &ParsedStdout<'_>) -> String {
    let Some(diagnostic) = parsed
        .json_documents()
        .ok()
        .and_then(|documents| wrapper_signals(documents).terminal_ending_diagnostic())
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

fn parse_json_envelope(
    exec_result: &ExecutionResult,
    parsed: &ParsedStdout<'_>,
) -> ResponseParseResult {
    let candidate = parsed.response_candidate()?;
    validate_envelope_frame(&candidate)?;
    let envelope = deserialize_envelope(candidate)?;
    let trace = synthesize_trace_from_parsed(exec_result, parsed);

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

// Visible through the `response` module to sibling-layout tests; keeping this private
// would require nesting tests back under `envelope`.
#[cfg(test)]
pub(in crate::types) fn synthesize_response(
    exec_result: &ExecutionResult,
) -> Option<(AgentResponseEnvelope, AgentResponseStatus, InvocationTrace)> {
    synthesize_from_parsed(exec_result, &ParsedStdout::parse(&exec_result.stdout))
}

fn synthesize_from_parsed(
    exec_result: &ExecutionResult,
    parsed: &ParsedStdout<'_>,
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
            synthesize_trace_from_parsed(exec_result, parsed),
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
    if let Some(diagnostic) = exit_zero_terminal_failure(exec_result, parsed) {
        return Some(synthesized_failure(
            exec_result,
            parsed,
            "AGENT_TERMINAL_ENDING",
            diagnostic,
        ));
    }

    if exec_result.exit_code.unwrap_or(1) == 0 || !exec_result.stdout.trim().is_empty() {
        return None;
    }

    Some(synthesized_failure(
        exec_result,
        parsed,
        "AGENT_INVOCATION_FAILED",
        synthetic_error_message(exec_result),
    ))
}

fn synthesized_failure(
    exec_result: &ExecutionResult,
    parsed: &ParsedStdout<'_>,
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
        synthesize_trace_from_parsed(exec_result, parsed),
    )
}

/// A clean exit whose wrapper nonetheless reports an abnormal ending, with no
/// envelope anywhere in stdout.
///
/// Requires the absence of an envelope: a real envelope is the authoritative
/// outcome whatever its status, and must never be displaced by a synthesized
/// one — that would let wrapper prose overrule the protocol.
fn exit_zero_terminal_failure(
    exec_result: &ExecutionResult,
    parsed: &ParsedStdout<'_>,
) -> Option<String> {
    if exec_result.exit_code.unwrap_or(1) != 0 {
        return None;
    }
    let documents = parsed.json_documents().ok()?;
    match parsed.discover(response_candidate) {
        Ok(None) => {}
        Ok(Some(_)) | Err(_) => return None,
    }
    wrapper_signals(documents).terminal_ending_diagnostic()
}

fn synthesize_trace_from_parsed(
    exec_result: &ExecutionResult,
    parsed: &ParsedStdout<'_>,
) -> InvocationTrace {
    match parsed.json_documents() {
        Ok(documents) => extract_invocation_trace(documents, exec_result.duration_ms),
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
const ENVELOPE_DISCOVERY_MAX_PARSE_ATTEMPTS: u32 = 4_096;

/// JSON nodes visited while searching for an envelope or declared failure.
const ENVELOPE_DISCOVERY_MAX_NODES: u32 = 65_536;

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
struct EnvelopeDiscoveryBudget {
    max_parse_attempts: u32,
    max_nodes: u32,
}

impl EnvelopeDiscoveryBudget {
    const fn production() -> Self {
        Self {
            max_parse_attempts: ENVELOPE_DISCOVERY_MAX_PARSE_ATTEMPTS,
            max_nodes: ENVELOPE_DISCOVERY_MAX_NODES,
        }
    }
}

/// Parse/traversal accounting for one bounded search.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct EnvelopeDiscoveryStats {
    parse_attempts: u32,
    nodes_visited: u32,
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

fn discover_in_values<'a, T, I, F>(
    values: I,
    budget: &mut Budget,
    inspect: &F,
) -> Result<Option<T>, OrbitError>
where
    I: IntoIterator<Item = &'a Value>,
    F: Fn(&Value) -> Option<T>,
{
    for value in values {
        if let Some(found) = walk(value, budget, inspect)? {
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

    // Scan forward to respect JSON container boundaries, then use the same
    // newest-first traversal as stdout documents. Searching backwards for `{`
    // would mistake a nested result or wrapper field for a later document.
    // The shared parse budget bounds this collection; exhaustion must fail
    // before any earlier envelope can become authoritative.
    let mut documents = Vec::new();
    let mut search_from = 0;
    while let Some(rel) = raw[search_from..].find('{') {
        let start = search_from + rel;
        budget.parse()?;
        let mut stream = Deserializer::from_str(&raw[start..]).into_iter::<Value>();
        match stream.next() {
            Some(Ok(nested)) => {
                let consumed = stream.byte_offset().max(1);
                documents.push(nested);
                search_from = start + consumed;
            }
            Some(Err(_)) | None => {
                search_from = start + 1;
            }
        }
    }
    discover_in_values(documents.iter().rev(), budget, inspect)
}

fn response_candidate(value: &Value) -> Option<Value> {
    let object = value.as_object()?;
    // Selection depends only on the frame keys. Invalid field types must be
    // rejected on this candidate, never make discovery return an older answer
    // or descend into a quoted envelope in its result/error payload.
    (object.contains_key("schemaVersion") && object.contains_key("status")).then(|| value.clone())
}

fn validate_envelope_frame(candidate: &Value) -> Result<(), OrbitError> {
    let schema_version = candidate
        .get("schemaVersion")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            OrbitError::AgentProtocolViolation(
                "schemaVersion must be a non-negative integer".to_string(),
            )
        })?;
    if schema_version != u64::from(RESPONSE_ENVELOPE_SCHEMA_VERSION) {
        return Err(OrbitError::AgentProtocolViolation(format!(
            "unsupported schemaVersion: {schema_version}"
        )));
    }
    let status = candidate
        .get("status")
        .and_then(Value::as_str)
        .ok_or_else(|| OrbitError::AgentProtocolViolation("status must be a string".to_string()))?;
    if !RESPONSE_ENVELOPE_STATUSES.contains(&status) {
        return Err(OrbitError::AgentProtocolViolation(format!(
            "unknown status: {status}"
        )));
    }
    Ok(())
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

fn deserialize_envelope(mut candidate: Value) -> Result<AgentResponseEnvelope, OrbitError> {
    // [ORB-10770] Claude's constrained decoder, given an untyped `error` and a
    // description that said "null when status is success", emits the JSON
    // *string* `"null"` (`jrun-20260813-0451-3`). `Option<AgentRunError>`
    // cannot accept a string, so the finder used to miss a completed
    // envelope. Treat `"null"` / `""` as absent; JSON null is already `None`
    // via `#[serde(default)]`. This only makes a recognizable frame parse —
    // it does not infer success from wrapper signals.
    if candidate
        .get("error")
        .and_then(Value::as_str)
        .is_some_and(is_absent_error_string)
        && let Some(object) = candidate.as_object_mut()
    {
        object.remove("error");
    }
    serde_json::from_value(candidate).map_err(|error| {
        OrbitError::AgentProtocolViolation(format!("invalid Orbit response envelope: {error}"))
    })
}

fn is_absent_error_string(token: &str) -> bool {
    let trimmed = token.trim();
    trimmed.is_empty() || trimmed == "null"
}
