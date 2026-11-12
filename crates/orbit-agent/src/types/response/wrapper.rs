//! Signals a provider CLI carries in the wrapper document *around* the Orbit
//! response envelope.
//!
//! [ORB-10746] Structured output removes the common cause of an exit-0 ending
//! with no envelope (a model answering in prose), but not the category. A
//! turn-limit ending still exits 0 with `is_error: true`,
//! `subtype: "error_max_turns"`, `terminal_reason: "max_turns"`, and both
//! `result` and `structured_output` null. Such runs would otherwise collapse into the
//! generic [ORB-10449] "stdout does not contain an Orbit response envelope"
//! message, which tells an operator nothing about why a full-cost run ended.
//!
//! Everything here is diagnostic only. These signals may sharpen a failure
//! *message*; they may never produce or imply a success. Provider exit status,
//! `is_error`, `subtype`, `terminal_reason`, and free-form prose are all
//! untrusted for that purpose — the envelope frame remains the sole authority
//! on whether an invocation completed its contract.

use serde_json::{Deserializer, Value};

/// Upper bound on provider prose quoted into a diagnostic. The CLI runner
/// bounds and redacts again at its own boundary; this keeps the string sane
/// for consumers that do not.
const QUOTED_PROSE_LIMIT_CHARS: usize = 400;

/// The wrapper fields Orbit reads for diagnostics. Deliberately a small,
/// named set: anything not listed here is not consulted when explaining a
/// failure.
#[derive(Debug, Default)]
pub(super) struct WrapperSignals {
    is_error: bool,
    subtype: Option<String>,
    terminal_reason: Option<String>,
    result_text: Option<String>,
}

/// Collect wrapper signals from the last document that carries any of them.
///
/// Providers emit the wrapper as their final document, so a reverse scan
/// finds the terminal record rather than an intermediate streaming event.
pub(super) fn wrapper_signals(documents: &[Value]) -> WrapperSignals {
    documents
        .iter()
        .rev()
        .find_map(|document| {
            let object = document.as_object()?;
            let carries_signal = ["is_error", "subtype", "terminal_reason"]
                .iter()
                .any(|key| object.contains_key(*key));
            if !carries_signal {
                return None;
            }
            Some(WrapperSignals {
                is_error: object
                    .get("is_error")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                subtype: non_empty_string(object.get("subtype")),
                terminal_reason: non_empty_string(object.get("terminal_reason")),
                result_text: non_empty_string(object.get("result")),
            })
        })
        .unwrap_or_default()
}

fn non_empty_string(value: Option<&Value>) -> Option<String> {
    let text = value?.as_str()?.trim();
    (!text.is_empty()).then(|| text.to_string())
}

fn quote_bounded(text: &str) -> String {
    let bounded: String = text.chars().take(QUOTED_PROSE_LIMIT_CHARS).collect();
    if bounded.chars().count() < text.chars().count() {
        format!("{bounded}…")
    } else {
        bounded
    }
}

impl WrapperSignals {
    /// Name a terminal ending the provider itself flagged as abnormal.
    ///
    /// Returns `None` when the wrapper reports an ordinary completion, so a
    /// run that merely failed to emit an envelope keeps the generic
    /// [ORB-10449] message rather than gaining a misleading cause.
    pub(super) fn terminal_ending_diagnostic(&self) -> Option<String> {
        let mut reasons = Vec::new();
        if self.is_error {
            reasons.push("is_error=true".to_string());
        }
        if let Some(subtype) = self.subtype.as_deref().filter(|value| *value != "success") {
            reasons.push(format!("subtype={subtype:?}"));
        }
        if let Some(reason) = self
            .terminal_reason
            .as_deref()
            .filter(|value| *value != "completed")
        {
            reasons.push(format!("terminal_reason={reason:?}"));
        }
        if reasons.is_empty() {
            return None;
        }

        let mut diagnostic = format!(
            "the provider reported an abnormal terminal ending ({}) and emitted no Orbit \
             response envelope",
            reasons.join(", ")
        );
        if let Some(prose) = self.result_text.as_deref() {
            diagnostic.push_str(&format!("; provider message: {}", quote_bounded(prose)));
        }
        Some(diagnostic)
    }

    /// Detect a provider that rejected the request outright — most relevantly
    /// Orbit's structured-output schema.
    ///
    /// Keys on `is_error` plus the wrapper's own prose. It deliberately does
    /// **not** key on `subtype`: in the observed schema-rejection shape
    /// (exit 1, `structured_output: null`, `result` = `API Error: 400
    /// tools.0.custom.input_schema: ...`) `subtype` still reads `"success"`,
    /// so treating it as an outcome discriminator would misclassify.
    fn request_rejection_diagnostic(&self) -> Option<String> {
        let prose = self.result_text.as_deref()?;
        if !self.is_error || !prose.contains("API Error") {
            return None;
        }
        if prose.contains("input_schema") {
            return Some(format!(
                "the provider rejected Orbit's response-envelope schema passed via \
                 --json-schema, so the invocation could not be constrained to the response \
                 protocol: {}",
                quote_bounded(prose)
            ));
        }
        Some(format!(
            "the provider returned an API error before completing the invocation: {}",
            quote_bounded(prose)
        ))
    }
}

/// Explain a *non-zero* provider exit in terms of the structured-output
/// contract, when the evidence supports a specific cause.
///
/// Two distinct failure shapes, distinguishable only by where they surface:
///
/// - A CLI that **lacks** `--json-schema` never starts work. Its argument
///   parser rejects the unknown option, so the evidence is on stderr and the
///   run costs nothing. This is the pre-flight capability failure.
/// - A CLI that **rejects** the schema fails mid-run, after the request
///   reaches the API. There is nothing wrong with the argv, so the evidence is
///   in the response wrapper.
///
/// Returns `None` when neither shape matches, leaving the caller's generic
/// exit-code message in place.
pub fn provider_invocation_diagnostic(stdout: &str, stderr: &str) -> Option<String> {
    if stderr.contains("unknown option") && stderr.contains("--json-schema") {
        return Some(format!(
            "the configured agent CLI does not support --json-schema and rejected it at argument \
             parsing, so no agent work ran: {}. Orbit enforces the response envelope through \
             structured output; upgrade the CLI or point the executor at a build that supports \
             it rather than running unconstrained.",
            quote_bounded(stderr.trim())
        ));
    }

    let documents = parse_documents(stdout)?;
    wrapper_signals(&documents).request_rejection_diagnostic()
}

/// Lenient re-parse for the diagnostic path. Unlike the protocol parser this
/// tolerates trailing junk: a best-effort explanation is worth more than
/// strictness on a stream that already failed.
fn parse_documents(stdout: &str) -> Option<Vec<Value>> {
    let documents: Vec<Value> = Deserializer::from_str(stdout)
        .into_iter::<Value>()
        .map_while(Result::ok)
        .collect();
    (!documents.is_empty()).then_some(documents)
}
