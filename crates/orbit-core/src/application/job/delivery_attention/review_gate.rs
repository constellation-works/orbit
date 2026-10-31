use orbit_types::workflow::REVIEW_ABANDONED_MARKER;
use serde_json::{Map, Value, json};

/// YAML step whose output and error carry the landing-review gate settlement.
pub const LANDING_REVIEW_SETTLE_STEP: &str = "landing_review_gate_settle";

/// Verdict and finding count recorded by the landing-review gate step.
///
/// `output` is that step's pipeline output. `step_error` is the step's own
/// error text, which is where a blocked settlement records the verdict and
/// the finding count. A known gate word (`awaiting_evidence`, and the other
/// settlements that are not a review verdict) is the displayed outcome when
/// the step recorded no verdict. The count stays absent when the step did
/// not record one; callers tell that from a missing verdict.
pub fn landing_review_gate_facts(output: Option<&Value>, step_error: Option<&str>) -> Value {
    let object = output.and_then(Value::as_object);
    let mut verdict = text_field(object, "verdict").map(str::to_string);
    let gate = text_field(object, "gate");
    let mut count = object.and_then(|object| finding_count(object.get("findings")));
    if let Some(message) = step_error.map(str::trim).filter(|text| !text.is_empty()) {
        if verdict.is_none() {
            verdict = verdict_from_message(message).map(str::to_string);
        }
        if count.is_none() {
            count = finding_count_from_message(message);
        }
    }
    let Some(shown) = verdict.or_else(|| gate.and_then(known_gate_label).map(str::to_string))
    else {
        return json!({ "availability": "unavailable" });
    };
    json!({
        "availability": "read",
        "verdict": shown,
        "finding_count": count,
    })
}

fn text_field<'a>(object: Option<&'a Map<String, Value>>, key: &str) -> Option<&'a str> {
    object
        .and_then(|object| object.get(key))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

fn finding_count(value: Option<&Value>) -> Option<u64> {
    let value = value?;
    if let Some(count) = value.as_u64() {
        return Some(count);
    }
    if let Some(count) = value.as_i64().filter(|count| *count >= 0) {
        return Some(count as u64);
    }
    value.as_array().map(|items| items.len() as u64)
}

fn verdict_from_message(message: &str) -> Option<&str> {
    // An abandoned settlement records no verdict: the step error says the
    // attempt "was released without a verdict". Report the gate outcome
    // instead of misreading the parenthetical "(verdict incomplete, ...)".
    // Only the settlement's lead counts: a blocked settlement may quote the
    // marker inside its escalation.
    if settlement_text(message).starts_with(REVIEW_ABANDONED_MARKER) {
        return Some("abandoned");
    }
    let rest = message.split("verdict ").nth(1)?;
    let token = rest
        .split([' ', '('])
        .next()?
        .trim()
        .trim_matches([',', ';', ':', '.', '!', '?', '"', '\'']);
    (!token.is_empty()).then_some(token)
}

/// The settlement text a step error carries. The refusal's `Display` prefixes
/// it with the refused action: ``deterministic action `<action>` refused: ``.
fn settlement_text(message: &str) -> &str {
    let message = message.trim_start();
    let settlement = message
        .strip_prefix("deterministic action `")
        .and_then(|rest| rest.split_once("` refused:"))
        .map_or(message, |(_, settlement)| settlement);
    settlement.trim_start()
}

fn finding_count_from_message(message: &str) -> Option<u64> {
    let marker = message
        .find(" finding(s) recorded")
        .or_else(|| message.find(" finding recorded"))?;
    message[..marker]
        .split_whitespace()
        .next_back()?
        .parse()
        .ok()
}

fn known_gate_label(gate: &str) -> Option<&'static str> {
    match gate {
        "awaiting_evidence" => Some("awaiting evidence"),
        "forge_unavailable" => Some("forge unavailable"),
        "not_required" => Some("not required"),
        "abandoned" => Some("abandoned"),
        "passed" => Some("passed"),
        _ => None,
    }
}
