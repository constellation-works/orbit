use std::path::Path;

use orbit_common::OrbitError;
use orbit_types::workflow::MergeMessage;
use serde_json::{Value, json};

use super::super::input::{optional_string, required_string, valid_expected_remote_sha};
use super::super::process::{execute, run_vcs_process, succeeded};
use super::super::{
    BASE_MODIFIED_REFUSAL, MERGE_CONFLICT_REFUSAL, MERGE_IN_PROGRESS_REFUSAL, SLOW_TIMEOUT_MS,
};

/// GitHub's reason for refusing a merge because another merge moved the base
/// between its mergeability check and the mutation (F2026-10-071). The `sha`
/// condition was not what failed.
const BASE_MODIFIED_REASON: &str = "Base branch was modified. Review and try the merge again.";

/// GitHub's reason for refusing a merge whose head no longer merges cleanly
/// into a base that advanced after its mergeability check (F2026-11-440).
const MERGE_CONFLICT_REASON: &str = "Pull Request has merge conflicts";

pub(in crate::executor::automation::vcs::operations) fn pr_merge(
    input: &Value,
) -> Result<Value, OrbitError> {
    let selector = required_string(input, "pr")?;
    let workspace_path = required_string(input, "workspace_path")?;
    let strategy = optional_string(input, "strategy").unwrap_or("squash");
    let strategy_flag = match strategy {
        "squash" => "--squash",
        "merge" => "--merge",
        "rebase" => "--rebase",
        other => {
            return Err(OrbitError::InvalidInput(format!(
                "invalid private automation VCS merge strategy '{other}'"
            )));
        }
    };
    // [ORB-25009] Orbit composes every squash and merge commit message; a
    // request without one would let the forge write its default.
    let message = merge_message(input, strategy)?;
    // `--auto` queues GitHub's own auto-merge, which still waits for every
    // required check and branch protection. There is deliberately no
    // administrative bypass (`--admin`) in this surface.
    let auto = input.get("auto").and_then(Value::as_bool).unwrap_or(false);
    if let Some(reviewed_head) = optional_string(input, "reviewed_head_sha") {
        if auto {
            return Err(OrbitError::InvalidInput(
                "review_gate_stale: deferred auto-merge cannot guarantee the reviewed head; wait for checks and request a synchronous merge".to_string(),
            ));
        }
        let flag = |name| input.get(name).and_then(Value::as_bool).unwrap_or(false);
        return pr_merge_reviewed(
            selector,
            workspace_path,
            strategy,
            message.as_ref(),
            reviewed_head,
            ReviewedMergeReports {
                base_modified: flag("report_base_modified"),
                ambiguous: flag("report_ambiguous"),
                conflict: flag("report_conflict"),
            },
        );
    }
    let mut args = vec![
        "pr".to_string(),
        "merge".to_string(),
        selector.to_string(),
        strategy_flag.to_string(),
    ];
    if let Some(message) = &message {
        args.extend([
            "--subject".to_string(),
            message.title.clone(),
            "--body".to_string(),
            message.body.clone(),
        ]);
    }
    if auto {
        args.push("--auto".to_string());
    }
    let result = execute(
        "gh",
        args,
        Some(Path::new(workspace_path)),
        SLOW_TIMEOUT_MS,
        "PR merge",
    )?;
    Ok(json!({
        "stdout": result.stdout,
        "stderr": result.stderr,
    }))
}

/// Which refusals a reviewed merge's caller asks to receive as values.
struct ReviewedMergeReports {
    base_modified: bool,
    ambiguous: bool,
    conflict: bool,
}

/// Use the synchronous REST mutation: `sha` is checked by GitHub when it
/// merges, and this endpoint never enables auto-merge or enters a merge queue.
/// `gh pr merge --match-head-commit` alone is insufficient because the CLI
/// can choose deferred semantics for a queue-required branch.
///
/// [ORB-14205] With `report_base_modified`, the provider's base-modification
/// refusal returns `{"merged": false, "refusal": "base_modified"}` with the
/// raw output, so the caller can decide whether to ask again. The exact
/// merge-in-progress refusal is a distinct value permitting reads, never a
/// second mutation. [F2026-11-440] With `report_conflict`, the exact
/// merge-conflict refusal returns `"refusal": "merge_conflict"`, which
/// permits one status read and never a second mutation. Other failures,
/// including a base-modification or conflict refusal without its opt-in,
/// stay errors unless
/// `report_ambiguous` opts into reporting an uncertain server response. That
/// outcome permits the completion caller one read, never a second mutation.
fn pr_merge_reviewed(
    selector: &str,
    workspace_path: &str,
    strategy: &str,
    message: Option<&MergeMessage>,
    reviewed_head: &str,
    reports: ReviewedMergeReports,
) -> Result<Value, OrbitError> {
    // Managed completion supplies a number in the current repository. Refuse
    // other selectors here rather than resolving a URL into a different repo.
    if selector.is_empty() || !selector.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(OrbitError::InvalidInput(
            "reviewed PR merge requires a pull request number in the workspace repository".into(),
        ));
    }
    if !valid_expected_remote_sha(Some(reviewed_head)) {
        return Err(OrbitError::InvalidInput(
            "reviewed PR merge requires an exact 40- or 64-character reviewed_head_sha".into(),
        ));
    }
    let mut args = vec![
        "api".to_string(),
        format!("repos/{{owner}}/{{repo}}/pulls/{selector}/merge"),
        "--method".to_string(),
        "PUT".to_string(),
        "-f".to_string(),
        format!("sha={reviewed_head}"),
        "-f".to_string(),
        format!("merge_method={strategy}"),
    ];
    // `-f` sends the raw string: a leading `@` is never read as a file.
    if let Some(message) = message {
        args.extend([
            "-f".to_string(),
            format!("commit_title={}", message.title),
            "-f".to_string(),
            format!("commit_message={}", message.body),
        ]);
    }
    let result = run_vcs_process("gh", args, Some(Path::new(workspace_path)), SLOW_TIMEOUT_MS)?;
    if reports.base_modified && is_base_modified_refusal(&result) {
        return Ok(json!({
            "merged": false,
            "refusal": BASE_MODIFIED_REFUSAL,
            "stdout": result.stdout,
            "stderr": result.stderr,
        }));
    }
    if result.exit_code.is_some() && is_exact_merge_refusal(&result, "Merge already in progress") {
        return Ok(json!({
            "merged": false,
            "refusal": MERGE_IN_PROGRESS_REFUSAL,
            "stdout": result.stdout,
            "stderr": result.stderr,
        }));
    }
    if reports.conflict
        && result.exit_code.is_some()
        && is_exact_merge_refusal(&result, MERGE_CONFLICT_REASON)
    {
        return Ok(json!({
            "merged": false,
            "refusal": MERGE_CONFLICT_REFUSAL,
            "stdout": result.stdout,
            "stderr": result.stderr,
        }));
    }
    let ambiguous = reports.ambiguous && is_ambiguous_merge_server_failure(&result);
    let result = match succeeded(result, "reviewed PR merge") {
        Err(error) if ambiguous => {
            return Ok(json!({
                "merged": false,
                "ambiguous_merge": error.to_string(),
            }));
        }
        result => result?,
    };
    let response: Value = serde_json::from_str(&result.stdout).map_err(|error| {
        OrbitError::Execution(format!("reviewed PR merge returned invalid JSON: {error}"))
    })?;
    if response.get("merged").and_then(Value::as_bool) != Some(true) {
        return Err(OrbitError::Execution(
            "reviewed PR merge did not confirm a synchronous merge; deferred merges are unsupported".into(),
        ));
    }
    let landed_commit = required_string(&response, "sha")?;
    Ok(json!({
        "stdout": result.stdout,
        "stderr": result.stderr,
        "landed_commit": landed_commit,
    }))
}

/// The Orbit-composed `commit_title` and `commit_message` a squash or merge
/// request must carry; `None` for a rebase, which writes no new message.
fn merge_message(input: &Value, strategy: &str) -> Result<Option<MergeMessage>, OrbitError> {
    if strategy == "rebase" {
        return Ok(None);
    }
    let title = optional_string(input, "commit_title")
        .map(str::trim)
        .filter(|title| !title.is_empty());
    let body = optional_string(input, "commit_message");
    match (title, body) {
        (Some(title), Some(body)) => Ok(Some(MergeMessage {
            title: title.to_string(),
            body: body.to_string(),
        })),
        _ => Err(OrbitError::InvalidInput(format!(
            "a {strategy} merge requires the Orbit-composed commit_title and commit_message; \
             the forge default message is never used"
        ))),
    }
}

/// True only for a completed `gh api` failure whose whole diagnostic is the
/// provider's base-modification refusal with HTTP 405: stderr is exactly
/// `gh: <reason> (HTTP 405)` and the response body, when present, is a JSON
/// error naming the same reason and status. Other 405s (policy, queue,
/// protection), other statuses, timeouts, and anything ambiguous do not match.
fn is_base_modified_refusal(result: &orbit_exec::ExecutionResult) -> bool {
    is_exact_merge_refusal(result, BASE_MODIFIED_REASON)
}

/// Match only one whole HTTP 405 diagnostic and a consistent optional body.
fn is_exact_merge_refusal(result: &orbit_exec::ExecutionResult, reason: &str) -> bool {
    if result.success
        || result.timed_out
        || result.stderr.trim() != format!("gh: {reason} (HTTP 405)")
    {
        return false;
    }
    let body = result.stdout.trim();
    if body.is_empty() {
        return true;
    }
    let Ok(Value::Object(body)) = serde_json::from_str::<Value>(body) else {
        return false;
    };
    body.get("message").and_then(Value::as_str) == Some(reason)
        && body
            .get("status")
            .is_none_or(|status| status.as_str() == Some("405") || status.as_u64() == Some(405))
}

/// Only completed synchronous server errors with an exact provider diagnostic
/// qualify for read-only reconciliation. Mixed policy/auth output, timeouts,
/// malformed bodies and successful but invalid responses retain their errors.
fn is_ambiguous_merge_server_failure(result: &orbit_exec::ExecutionResult) -> bool {
    if result.success || result.timed_out || result.exit_code.is_none() {
        return false;
    }
    for (status, reason) in [
        (500, "Internal Server Error"),
        (502, "Server Error"),
        (502, "Bad Gateway"),
        (503, "Service Unavailable"),
        (504, "Gateway Timeout"),
    ] {
        if result.stderr.trim() != format!("gh: {reason} (HTTP {status})") {
            continue;
        }
        if result.stdout.trim().is_empty() {
            return true;
        }
        let Ok(Value::Object(body)) = serde_json::from_str::<Value>(&result.stdout) else {
            return false;
        };
        return body
            .keys()
            .all(|key| matches!(key.as_str(), "message" | "status" | "documentation_url"))
            && body.get("message").and_then(Value::as_str) == Some(reason)
            && body.get("status").is_none_or(|value| {
                value.as_u64() == Some(status)
                    || value.as_str() == Some(status.to_string().as_str())
            });
    }
    false
}
