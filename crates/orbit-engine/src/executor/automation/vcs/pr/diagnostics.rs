//! Bounded, non-authoritative observations for owner landing refusals.
//! Nothing in this module performs provider reads or selects landing actions.

use orbit_common::security::redaction::redact_all;
use serde_json::{Value, json};

use super::answers::{PullRequestStatus, ReviewDecision, StatusCheck};

const MAX_ITEMS: usize = 32;
const MAX_TEXT_CHARS: usize = 512;

/// Redact the complete string before truncating, so a token crossing the
/// truncation boundary cannot escape redaction. Counts describe original input.
pub(in crate::executor::automation::vcs) fn text(raw: &str) -> Value {
    let redacted = redact_all(raw);
    json!({
        "value": redacted.chars().take(MAX_TEXT_CHARS).collect::<String>(),
        "original_chars": raw.chars().count(),
        "truncated": redacted.chars().count() > MAX_TEXT_CHARS,
    })
}

/// Recovery reasons already carry bounded excerpts. Keep a larger, bounded
/// human reason and its final disposition so diagnostics do not crowd it out.
pub(in crate::executor::automation::vcs) fn reason(raw: &str) -> Value {
    const LIMIT: usize = 4096;
    const MARKER: &str = "\n[… truncated …]\n";
    let redacted = redact_all(raw);
    let count = redacted.chars().count();
    let value = if count <= LIMIT {
        redacted
    } else {
        let mut value = redacted.chars().take(3072).collect::<String>();
        value.push_str(MARKER);
        value.extend(
            redacted
                .chars()
                .skip(count - (LIMIT - 3072 - MARKER.chars().count())),
        );
        value
    };
    json!({"value":value,"original_chars":raw.chars().count(),"truncated":count > LIMIT})
}

fn optional_text(raw: Option<&str>) -> Value {
    raw.map(text).unwrap_or_else(|| json!({"state":"missing"}))
}

pub(in crate::executor::automation::vcs) fn collection<T>(
    items: &[T],
    project: impl Fn(&T) -> Value,
) -> Value {
    json!({"state":"collected", "original_count":items.len(),
        "truncated": items.len() > MAX_ITEMS,
        "items":items.iter().take(MAX_ITEMS).map(project).collect::<Vec<_>>()})
}

fn check(check: &StatusCheck) -> Value {
    json!({"name":optional_text(check.name.as_deref()),
        "workflow":optional_text(check.workflow_name.as_deref()),
        "state":check.state.map(|state| format!("{state:?}")).unwrap_or_else(|| "unknown".into()),
        "owed_state":check.owed_state.map(|state| format!("{state:?}")).unwrap_or_else(|| "unknown".into())})
}

pub(in crate::executor::automation::vcs) fn status(status: &PullRequestStatus) -> Value {
    let review = match &status.review_decision {
        ReviewDecision::NotApplicable => json!({"state":"not_applicable"}),
        ReviewDecision::Unavailable => json!({"state":"unavailable"}),
        ReviewDecision::Approved
        | ReviewDecision::ReviewRequired
        | ReviewDecision::ChangesRequested
        | ReviewDecision::Other(_) => text(
            status
                .review_decision
                .provider_spelling()
                .unwrap_or_default(),
        ),
    };
    json!({"state":"collected", "number":status.number,
    "head":optional_text(status.head_sha.as_deref()),
    "head_branch":optional_text(status.head_branch.as_deref()),
    "base_branch":optional_text(status.base_branch.as_deref()),
    "provider_state":optional_text(status.state.as_deref()),
    "merge_state":optional_text(status.merge_state_status.as_deref()),
    "is_draft":status.is_draft, "review":review,
    "rollup":status.status_checks.as_deref().map(|checks| collection(checks, check))
        .unwrap_or_else(|| json!({"state":match status.provider_state().get("statusCheckRollup") {
            None => "missing",
            Some(Value::Null) => "null",
            _ => "unreadable",
        }}))})
}

/// An allowlisted projection of the actual required-check response, including
/// malformed/unknown fields. Never retain arbitrary payloads, URL fields, or logs.
/// Classification still uses RequiredChecks::from_response, independently.
pub(super) fn required_response(response: &Value) -> Value {
    let pr = response.get("pull_request");
    let commit = pr.and_then(|pr| pr.pointer("/commits/nodes/0/commit"));
    let rollup = commit.and_then(|commit| commit.get("statusCheckRollup"));
    let contexts = rollup.and_then(|rollup| rollup.get("contexts"));
    let nodes = contexts.and_then(|contexts| contexts.get("nodes"));
    let field = |value: Option<&Value>| match value {
        None => json!({"state":"missing"}),
        Some(Value::Null) => json!({"state":"null"}),
        Some(Value::String(value)) => text(value),
        Some(Value::Bool(value)) => json!({"value":value}),
        _ => json!({"state":"invalid_type"}),
    };
    json!({"state":"collected",
    "reported_head":field(pr.and_then(|pr| pr.get("headRefOid"))),
    "commit_head":field(commit.and_then(|commit| commit.get("oid"))),
    "has_next_page":field(contexts.and_then(|contexts| contexts.pointer("/pageInfo/hasNextPage"))),
    "rollup_state": match rollup {
        None => "missing",
        Some(Value::Null) => "null",
        Some(Value::Object(_)) => "object",
        _ => "invalid_type",
    },
    "contexts": match nodes {
        Some(Value::Array(nodes)) => collection(nodes, |node| {
            let mut entry = check(&StatusCheck::parse(node));
            entry["workflow"] = field(node.pointer("/checkSuite/workflowRun/workflow/name"));
            entry["is_required"] = field(node.get("isRequired"));
            entry["provider_status"] = field(node.get("status"));
            entry["provider_conclusion"] = field(node.get("conclusion"));
            entry["provider_state"] = field(node.get("state"));
            entry
        }),
        _ => field(nodes),
    }})
}
