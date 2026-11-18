//! Source-repair identity comparison against a pull-request answer.
//!
//! The predicate is pure: it names no provider operation and no higher VCS
//! module. Publication settling stays with its caller and uses these
//! projections so refused-read evidence stays the same.

use orbit_common::{security::redaction::redact_all, text::floor_char_boundary};
use orbit_types::workflow::SourceRepairBinding;
use serde_json::{Value, json};

use super::answers::PullRequestStatus;

/// Compare before projecting/redacting evidence. Missing GraphQL merge metadata
/// cannot establish absence of a merge; REST provisional merge SHAs are ignored.
pub(in crate::executor::automation::vcs) fn mismatches(
    binding: &SourceRepairBinding,
    pr: &PullRequestStatus,
    expected_head: &str,
) -> Vec<Value> {
    let raw = pr.provider_state();
    if !pr.identity_metadata_readable {
        return vec![
            json!({"field":"observation", "expected":"pull_request object", "observed":{"presence":"invalid", "value":bounded_publication_value(raw)}}),
        ];
    }
    let checks = [
        (
            "number",
            binding
                .pull_request
                .parse::<u64>()
                .map_or_else(|_| json!(binding.pull_request), |number| json!(number)),
            pr.number.map(|n| n.to_string()).as_deref() == Some(binding.pull_request.as_str()),
        ),
        (
            "url",
            json!(binding.pull_request_url),
            pr.url.as_deref() == Some(binding.pull_request_url.as_str()),
        ),
        ("state", json!("OPEN"), pr.state.as_deref() == Some("OPEN")),
        (
            "headRefName",
            json!(binding.branch),
            pr.head_branch.as_deref() == Some(binding.branch.as_str()),
        ),
        (
            "headRefOid",
            json!(expected_head),
            pr.head_sha.as_deref() == Some(expected_head),
        ),
        (
            "baseRefName",
            json!(binding.base_branch),
            pr.base_branch.as_deref() == Some(binding.base_branch.as_str()),
        ),
        (
            "autoMergeRequest",
            Value::Null,
            pr.auto_merge_enabled == Some(false),
        ),
        (
            "mergeCommit",
            Value::Null,
            pr.merge_commit_absent && pr.merge_commit.is_none(),
        ),
        (
            "mergedAt",
            Value::Null,
            pr.merged_at_absent && pr.merged_at.is_none(),
        ),
    ];
    checks.into_iter().filter(|(_, _, matches)| !matches).map(|(field, expected, _)| {
        json!({"field":field, "expected":bounded_publication_value(&expected), "observed":publication_field_observed(raw.get(field))})
    }).collect()
}

/// One provider field as present or missing, after the bounded projection.
fn publication_field_observed(value: Option<&Value>) -> Value {
    match value {
        Some(value) => json!({"presence":"present", "value":bounded_publication_value(value)}),
        None => json!({"presence":"missing"}),
    }
}

/// Fixed-depth projection of hostile provider values. Objects/arrays are never
/// copied wholesale; only a merge commit's oid can be useful identity evidence.
pub(in crate::executor::automation::vcs) fn bounded_publication_value(value: &Value) -> Value {
    match value {
        Value::String(text) => redacted_bounded_text(text),
        Value::Object(object) => {
            json!({"type":"object", "oid":object.get("oid").map(|oid| match oid { Value::String(s) => redacted_bounded_text(s), _ => json!({"type":"invalid"}) })})
        }
        Value::Array(_) => json!({"type":"array"}),
        other => other.clone(),
    }
}

/// Bound and redact one text field. Ordinary pull-request URLs stay; userinfo,
/// query and fragment payloads do not, including text that was supposed to be
/// a branch name. Longer than 512 bytes, the value keeps a prefix and its
/// original length.
pub(in crate::executor::automation::vcs) fn redacted_bounded_text(text: &str) -> Value {
    // Retain ordinary PR URLs, but no userinfo, query or fragment payloads in
    // arbitrary text, including fields that were supposed to be branch names.
    let safe = if text.contains("://") && text.contains(['@', '?', '#']) {
        "[REDACTED_LOCATION]".into()
    } else {
        redact_all(text)
    };
    const FIELD_BYTES: usize = 512;
    if safe.len() > FIELD_BYTES {
        json!({"prefix":&safe[..floor_char_boundary(&safe, FIELD_BYTES)], "bytes":safe.len(), "truncated":true})
    } else {
        json!(safe)
    }
}
