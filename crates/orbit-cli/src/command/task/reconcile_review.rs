//! `orbit task reconcile-review` rendering, here or routed to another host.

use serde_json::Value;

/// The text beside a reconciliation answer: eligibility and the binding it
/// would reconcile, then one line per reconciliation.
pub(crate) fn reconciliation_text(value: &Value) -> String {
    let mut lines = Vec::new();
    if let Some(eligible) = value.get("eligible").and_then(Value::as_bool) {
        lines.push(format!("Eligible: {eligible}"));
        for (label, pointer) in [
            ("Refusal", "/refusal"),
            ("Next step", "/next_step"),
            ("Run", "/binding/execution/run_id"),
            ("Host", "/binding/execution/machine_id"),
            ("Claim", "/binding/execution/claim_id"),
            ("Handoff", "/binding/execution/handoff_id"),
            ("Candidate", "/binding/execution/candidate_commit"),
            ("Pull request", "/binding/pull_request/number"),
            ("URL", "/binding/pull_request/url"),
            ("Repository", "/binding/pull_request/repository"),
            ("Landing branch", "/binding/pull_request/landing_branch"),
            ("Merged head", "/binding/pull_request/merged_head/commit"),
            ("Base", "/binding/pull_request/base/commit"),
            ("Landed commit", "/binding/pull_request/landed/commit"),
            ("Commands source", "/contract/commands_source"),
            ("Accepted commands", "/contract/accepted_commands"),
            ("Required commands", "/contract/required_commands"),
            ("Review crew", "/contract/review_crew"),
            ("Review crew source", "/contract/review_crew_source"),
            ("Frozen at", "/contract/frozen_at"),
        ] {
            if let Some(field) = value.pointer(pointer).filter(|field| !field.is_null()) {
                let text = field
                    .as_str()
                    .map_or_else(|| field.to_string(), str::to_owned);
                lines.push(format!("{label}: {text}"));
            }
        }
    }
    if let Some(records) = value.get("reconciliations").and_then(Value::as_array) {
        if records.is_empty() {
            lines.push("No reconciliations.".into());
        }
        lines.extend(records.iter().map(reconciliation_line));
    } else if value.get("reconciliation_id").is_some() {
        lines.push(reconciliation_line(value));
    }
    lines.join("\n")
}

fn reconciliation_line(value: &Value) -> String {
    let field = |name| value.get(name).and_then(Value::as_str);
    let mut line = format!(
        "{}: {}",
        field("reconciliation_id").unwrap_or("-"),
        field("outcome").unwrap_or("unsettled"),
    );
    if let Some(run) = field("run_id") {
        line.push_str(&format!("; run: {run}"));
        if let Some(state) = field("run_state") {
            line.push_str(&format!(" ({state})"));
        }
    }
    if value.get("replayed").and_then(Value::as_bool) == Some(true) {
        line.push_str("; replayed");
    }
    if let Some(next) = field("next_step") {
        line.push_str(&format!("; next step: {next}"));
    }
    line
}
