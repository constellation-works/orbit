use std::path::Path;

use orbit_common::OrbitError;
use serde_json::{Value, json};

use super::super::input::{optional_string, required_string, valid_expected_remote_sha};
use super::super::process::{execute, run_vcs_process, succeeded};
use super::super::{BASE_MODIFIED_REFUSAL, SLOW_TIMEOUT_MS};

/// GitHub's reason for refusing a merge because another merge moved the base
/// between its mergeability check and the mutation (F2026-10-071). The `sha`
/// condition was not what failed.
const BASE_MODIFIED_REASON: &str = "Base branch was modified. Review and try the merge again.";

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
        let report_base_modified = input
            .get("report_base_modified")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        return pr_merge_reviewed(
            selector,
            workspace_path,
            strategy,
            reviewed_head,
            report_base_modified,
        );
    }
    let mut args = vec![
        "pr".to_string(),
        "merge".to_string(),
        selector.to_string(),
        strategy_flag.to_string(),
    ];
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

/// Use the synchronous REST mutation: `sha` is checked by GitHub when it
/// merges, and this endpoint never enables auto-merge or enters a merge queue.
/// `gh pr merge --match-head-commit` alone is insufficient because the CLI
/// can choose deferred semantics for a queue-required branch.
///
/// [ORB-14205] With `report_base_modified`, the provider's base-modification
/// refusal returns `{"merged": false, "refusal": "base_modified"}` with the
/// raw output, so the caller can decide whether to ask again. Every other
/// failure, and that one without the opt-in, stays an error.
fn pr_merge_reviewed(
    selector: &str,
    workspace_path: &str,
    strategy: &str,
    reviewed_head: &str,
    report_base_modified: bool,
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
    let result = run_vcs_process(
        "gh",
        vec![
            "api".to_string(),
            format!("repos/{{owner}}/{{repo}}/pulls/{selector}/merge"),
            "--method".to_string(),
            "PUT".to_string(),
            "-f".to_string(),
            format!("sha={reviewed_head}"),
            "-f".to_string(),
            format!("merge_method={strategy}"),
        ],
        Some(Path::new(workspace_path)),
        SLOW_TIMEOUT_MS,
    )?;
    if report_base_modified && is_base_modified_refusal(&result) {
        return Ok(json!({
            "merged": false,
            "refusal": BASE_MODIFIED_REFUSAL,
            "stdout": result.stdout,
            "stderr": result.stderr,
        }));
    }
    let result = succeeded(result, "reviewed PR merge")?;
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

/// True only for a completed `gh api` failure whose whole diagnostic is the
/// provider's base-modification refusal with HTTP 405: stderr is exactly
/// `gh: <reason> (HTTP 405)` and the response body, when present, is a JSON
/// error naming the same reason and status. Other 405s (policy, queue,
/// protection), other statuses, timeouts, and anything ambiguous do not match.
fn is_base_modified_refusal(result: &orbit_exec::ExecutionResult) -> bool {
    if result.success
        || result.timed_out
        || result.stderr.trim() != format!("gh: {BASE_MODIFIED_REASON} (HTTP 405)")
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
    body.get("message").and_then(Value::as_str) == Some(BASE_MODIFIED_REASON)
        && body
            .get("status")
            .is_none_or(|status| status.as_str() == Some("405") || status.as_u64() == Some(405))
}
