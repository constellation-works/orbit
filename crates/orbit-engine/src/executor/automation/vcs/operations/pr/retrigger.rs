//! [ORB-23812] Re-request hosted checks for a pull request head the forge
//! started no workflow run for.
//!
//! GitHub can drop the `pull_request` event of a push, leaving the head with
//! no run of any required check. Closing and reopening the pull request emits
//! a `pull_request` `reopened` event for the same head. It changes neither
//! the head, nor the branch, nor any review, and the pull request is open and
//! ready again when this returns. Nothing is requested when the head moved,
//! when the pull request is not open and ready, or when any workflow run
//! already exists for the head.

use std::path::Path;

use orbit_common::OrbitError;
use serde_json::{Value, json};

use super::super::input::{required_string, valid_expected_remote_sha};
use super::super::process::{execute, execute_with_transient_retry};
use super::super::{DEFAULT_TIMEOUT_MS, GITHUB_TRANSIENT_ATTEMPTS, GITHUB_TRANSIENT_RETRY_DELAY};

/// What the pull request reported before or after the re-trigger.
struct Observed {
    state: String,
    draft: bool,
    head_sha: String,
}

fn observe(workspace: &Path, number: &str) -> Result<Observed, OrbitError> {
    let result = execute_with_transient_retry(
        "gh",
        &[
            "pr".to_string(),
            "view".to_string(),
            number.to_string(),
            "--json".to_string(),
            "state,isDraft,headRefOid".to_string(),
        ],
        Some(workspace),
        DEFAULT_TIMEOUT_MS,
        "PR check re-trigger view",
    )?;
    let view: Value = serde_json::from_str(&result.stdout).map_err(|error| {
        OrbitError::Execution(format!(
            "private automation VCS PR check re-trigger view returned invalid JSON: {error}"
        ))
    })?;
    let field = |name: &str| {
        view[name].as_str().map(ToOwned::to_owned).ok_or_else(|| {
            OrbitError::Execution(format!(
                "pull request #{number} did not report its {name} for a check re-trigger"
            ))
        })
    };
    Ok(Observed {
        state: field("state")?,
        draft: view["isDraft"].as_bool().unwrap_or(false),
        head_sha: field("headRefOid")?,
    })
}

/// Workflow runs the forge reports for exactly `head_sha`, in any state.
fn head_workflow_runs(workspace: &Path, head_sha: &str) -> Result<u64, OrbitError> {
    let result = execute_with_transient_retry(
        "gh",
        &[
            "api".to_string(),
            format!("repos/{{owner}}/{{repo}}/actions/runs?head_sha={head_sha}&per_page=1"),
        ],
        Some(workspace),
        DEFAULT_TIMEOUT_MS,
        "PR check re-trigger run list",
    )?;
    let runs: Value = serde_json::from_str(&result.stdout).map_err(|error| {
        OrbitError::Execution(format!(
            "private automation VCS workflow run list returned invalid JSON: {error}"
        ))
    })?;
    runs["total_count"].as_u64().ok_or_else(|| {
        OrbitError::Execution("the workflow run list carried no total_count".to_string())
    })
}

/// Re-request hosted checks on open pull request `pr` at exactly
/// `head_sha`. Answers `{"requested": false, "reason": ...}` when the forge
/// already has a workflow run for the head, and errors before any mutation
/// when the pull request is not open and ready at that head. After the
/// mutation it confirms the pull request is open again on the same head.
pub(in crate::executor::automation::vcs::operations) fn pr_retrigger_checks(
    input: &Value,
) -> Result<Value, OrbitError> {
    let number = required_string(input, "pr")?;
    let head_sha = required_string(input, "head_sha")?;
    let workspace = Path::new(required_string(input, "workspace_path")?);
    if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(OrbitError::InvalidInput(format!(
            "invalid private automation VCS PR check re-trigger selector '{number}'; expected a \
             pull request number in the workspace repository"
        )));
    }
    if !valid_expected_remote_sha(Some(head_sha)) {
        return Err(OrbitError::InvalidInput(
            "PR check re-trigger requires an exact 40- or 64-character head_sha".into(),
        ));
    }
    let before = observe(workspace, number)?;
    if before.state != "OPEN" || before.draft || before.head_sha != head_sha {
        return Err(OrbitError::Execution(format!(
            "pull request #{number} is not open and ready at head {head_sha} (state {}, draft \
             {}, head {}); no checks were re-requested",
            before.state, before.draft, before.head_sha
        )));
    }
    let runs = head_workflow_runs(workspace, head_sha)?;
    if runs > 0 {
        return Ok(json!({
            "requested": false,
            "reason": "workflow_runs_present",
            "head_sha": head_sha,
        }));
    }
    // Single-shot close: a reopen follows unconditionally, so an ambiguous
    // close reply still ends with the pull request open.
    let closed = execute(
        "gh",
        vec!["pr".to_string(), "close".to_string(), number.to_string()],
        Some(workspace),
        DEFAULT_TIMEOUT_MS,
        "PR check re-trigger close",
    );
    let mut reopened = None;
    for attempt in 1..=GITHUB_TRANSIENT_ATTEMPTS {
        match execute(
            "gh",
            vec!["pr".to_string(), "reopen".to_string(), number.to_string()],
            Some(workspace),
            DEFAULT_TIMEOUT_MS,
            "PR check re-trigger reopen",
        ) {
            Ok(_) => {
                reopened = None;
                break;
            }
            Err(error) => {
                reopened = Some(error);
                if attempt < GITHUB_TRANSIENT_ATTEMPTS {
                    super::super::super::remote_probe::fixed_wait(GITHUB_TRANSIENT_RETRY_DELAY);
                }
            }
        }
    }
    let after = observe(workspace, number)?;
    if after.state != "OPEN" {
        let cause = reopened
            .map(|error| error.to_string())
            .or_else(|| closed.as_ref().err().map(ToString::to_string))
            .unwrap_or_default();
        return Err(OrbitError::Execution(format!(
            "pull request #{number} is {} after its check re-trigger and must be reopened by \
             hand: {cause}",
            after.state
        )));
    }
    if let Err(error) = closed {
        return Err(OrbitError::Execution(format!(
            "pull request #{number} did not close for its check re-trigger: {error}"
        )));
    }
    if after.head_sha != head_sha {
        return Err(OrbitError::Execution(format!(
            "pull request #{number} reports head {} after its check re-trigger, not {head_sha}",
            after.head_sha
        )));
    }
    Ok(json!({
        "requested": true,
        "method": "reopen",
        "head_sha": head_sha,
    }))
}
