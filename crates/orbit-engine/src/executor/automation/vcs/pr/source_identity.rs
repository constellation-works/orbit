//! Exact publication predicates and bounded evidence for refused provider reads.
use chrono::Utc;
use orbit_common::{
    OrbitError, security::redaction::redact_all, security::release::sha256_hex,
    text::floor_char_boundary,
};
use orbit_types::{
    task::TaskHistoryEntry,
    workflow::{JobRunState, SourceRepairBinding},
};
use serde_json::{Value, json};

use super::super::{git::git_output, handoff::HandoffContext, operations};
use super::{PullRequestStatus, read_pr_status};
use crate::context::{RuntimeHost, TaskAutomationUpdate};

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
            json!({"field":"observation", "expected":"pull_request object", "observed":{"presence":"invalid", "value":bounded_value(raw)}}),
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
        json!({"field":field, "expected":bounded_value(&expected), "observed":observed(raw.get(field))})
    }).collect()
}

/// Keep the read and its successful push provenance together before returning
/// the refusal. Unlike handoff comments, history records are not deduplicated
/// by run/phase, so a later refused observation cannot erase the earlier one.
pub(super) fn verify_publication<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &HandoffContext,
    binding: &SourceRepairBinding,
    head: &str,
    base: &str,
) -> Result<(), OrbitError> {
    let started_at = Utc::now();
    let read = read_pr_status(host, &context.workspace_path, &binding.pull_request);
    let finished_at = Utc::now();
    // Preserve the publication guard's ordering: compare the PR observation
    // with HEAD read after it, rather than a local head that preceded the read.
    let published = git_output(&context.workspace_path, &["rev-parse", "HEAD"])?;
    let published_head = published.trim();
    let push = successful_push(host, &context.batch_id, binding, published_head);
    let mut mismatches = match &read {
        Ok(pr) => mismatches(binding, pr, published_head),
        Err(_) => vec![
            json!({"field":"observation", "expected":"readable pull_request object", "observed":{"presence":"unreadable"}}),
        ],
    };
    for (field, expected, observed) in [
        ("prepared_head_branch", binding.branch.as_str(), head),
        ("prepared_base_branch", binding.base_branch.as_str(), base),
    ] {
        if expected != observed {
            mismatches.push(json!({"field":field, "expected":safe_text(expected), "observed":{"presence":"present", "value":safe_text(observed)}}));
        }
    }
    if mismatches.is_empty() {
        return Ok(());
    }
    // Only a fixed identity allowlist is retained: no remote credentials,
    // diagnosis, body, checks, error streams or auto-merge actor payloads.
    let mut admission = serde_json::Map::new();
    let binding_json =
        serde_json::to_value(binding).map_err(|e| OrbitError::Execution(e.to_string()))?;
    for key in [
        "workspace_id",
        "task_id",
        "source_run_id",
        "machine_id",
        "source_digest",
        "task_digest",
        "repository",
        "branch",
        "pull_request",
        "pull_request_url",
        "head_sha",
        "base_sha",
        "base_branch",
    ] {
        admission.insert(key.into(), bounded_value(&binding_json[key]));
    }
    let fields = mismatches
        .iter()
        .filter_map(|m| m["field"].as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let evidence = json!({
        "schema_version":1,
        "run_id":safe_text(&context.batch_id),
        "admission":admission,
        "admission_binding_sha256":sha256_hex(&serde_json::to_vec(binding).map_err(|e| OrbitError::Execution(e.to_string()))?),
        "published_head":safe_text(published_head),
        "successful_push":push,
        "read":{"operation":operations::PR_STATUS, "selector":safe_text(&binding.pull_request), "started_at":started_at, "finished_at":finished_at,
            "outcome":if read.is_ok() { "readable" } else { "unreadable" },
            "failure_kind":read.as_ref().err().map(|error| if error.is_timeout() { "timeout" } else { "provider_read_failed" })},
        "mismatches":mismatches,
    });
    host.apply_task_automation_update(
        &binding.task_id,
        TaskAutomationUpdate {
            append_history: vec![TaskHistoryEntry {
                at: finished_at,
                by: "system".into(),
                event: "source_repair_publication_refused".into(),
                note: Some(evidence.to_string()),
                from_status: None,
                to_status: None,
                operation_id: None,
                decision: None,
            }],
            ..Default::default()
        },
    )?;
    // Preserve timeout classification and its ordinary budget handling.
    if let Err(error) = read
        && error.is_timeout()
    {
        return Err(error);
    }
    Err(OrbitError::Execution(format!(
        "source repair PR identity changed ({fields}); creating or replacing a pull request is not authorized; expected/observed evidence retained in source_repair_publication_refused history"
    )))
}

/// A checkpoint is evidence only when this run's successful step output agrees
/// with its named pipeline checkpoint and the exact branch/published head.
/// Missing or unreadable provenance is recorded as unconfirmed, never invented
/// from the caller's input or the PR answer. This does not authorize a push.
fn successful_push<H: RuntimeHost + ?Sized>(
    host: &H,
    run_id: &str,
    binding: &SourceRepairBinding,
    head: &str,
) -> Value {
    let Ok(Some(state)) = host.read_run_state(run_id) else {
        return Value::Null;
    };
    if state.run_id != run_id || state.job_id != "task_pr_pipeline" {
        return Value::Null;
    }
    let Some(push) = state.pipeline.get("push") else {
        return Value::Null;
    };
    if push["phase"] != "push" || push["branch"] != binding.branch || push["local_sha"] != head {
        return Value::Null;
    }
    let Some((index, _)) = state.step_outputs.iter().find(|(index, output)| {
        *output == push && state.step_states.get(index) == Some(&JobRunState::Success)
    }) else {
        return Value::Null;
    };
    json!({"step_index":index, "checkpoint":"push", "checkpoint_sha256":sha256_hex(push.to_string().as_bytes()), "branch":bounded_value(&push["branch"]), "local_sha":bounded_value(&push["local_sha"]), "remote_sha_before":bounded_value(&push["remote_sha_before"]), "force_with_lease":push["force_with_lease"].as_bool(), "decision":bounded_value(&push["decision"])})
}

fn observed(value: Option<&Value>) -> Value {
    match value {
        Some(value) => json!({"presence":"present", "value":bounded_value(value)}),
        None => json!({"presence":"missing"}),
    }
}

/// Fixed-depth projection of hostile provider values. Objects/arrays are never
/// copied wholesale; only a merge commit's oid can be useful identity evidence.
fn bounded_value(value: &Value) -> Value {
    match value {
        Value::String(text) => safe_text(text),
        Value::Object(object) => {
            json!({"type":"object", "oid":object.get("oid").map(|oid| match oid { Value::String(s) => safe_text(s), _ => json!({"type":"invalid"}) })})
        }
        Value::Array(_) => json!({"type":"array"}),
        other => other.clone(),
    }
}

fn safe_text(text: &str) -> Value {
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
