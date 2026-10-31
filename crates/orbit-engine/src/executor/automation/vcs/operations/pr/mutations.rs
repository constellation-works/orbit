use std::path::Path;

use orbit_common::OrbitError;
use serde_json::{Value, json};

use super::super::input::{required_string, valid_expected_remote_sha};
use super::super::process::execute;
use super::super::{DEFAULT_TIMEOUT_MS, SLOW_TIMEOUT_MS};

/// [ORB-15308] Close one pull request of this repository, by number, with a
/// comment saying why. Single-shot: a retry after an ambiguous failure could
/// post the comment twice.
pub(in crate::executor::automation::vcs::operations) fn pr_close(
    input: &Value,
) -> Result<Value, OrbitError> {
    let number = required_string(input, "pr")?;
    let comment = required_string(input, "comment")?;
    let workspace_path = required_string(input, "workspace_path")?;
    if !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(OrbitError::InvalidInput(format!(
            "invalid private automation VCS PR close selector '{number}'; expected a pull \
             request number in the workspace repository"
        )));
    }
    let args = vec![
        "pr".to_string(),
        "close".to_string(),
        number.to_string(),
        "--comment".to_string(),
        comment.to_string(),
    ];
    let result = execute(
        "gh",
        args,
        Some(Path::new(workspace_path)),
        DEFAULT_TIMEOUT_MS,
        "PR close",
    )?;
    Ok(json!({
        "stdout": result.stdout,
        "stderr": result.stderr,
    }))
}

/// [ORB-15956] Merge the base branch's current tip into a pull request's
/// branch, as the provider's "update branch" does. `expected_head_sha` makes
/// the provider refuse a head that moved after the caller read it.
/// Single-shot: a retry after an ambiguous failure could merge twice.
pub(in crate::executor::automation::vcs::operations) fn pr_update_branch(
    input: &Value,
) -> Result<Value, OrbitError> {
    let number = required_string(input, "pr")?;
    let expected_head = required_string(input, "expected_head_sha")?;
    let workspace_path = required_string(input, "workspace_path")?;
    if !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(OrbitError::InvalidInput(format!(
            "invalid private automation VCS PR update selector '{number}'; expected a pull \
             request number in the workspace repository"
        )));
    }
    if !valid_expected_remote_sha(Some(expected_head)) {
        return Err(OrbitError::InvalidInput(
            "PR branch update requires an exact 40- or 64-character expected_head_sha".into(),
        ));
    }
    let result = execute(
        "gh",
        vec![
            "api".to_string(),
            format!("repos/{{owner}}/{{repo}}/pulls/{number}/update-branch"),
            "--method".to_string(),
            "PUT".to_string(),
            "-f".to_string(),
            format!("expected_head_sha={expected_head}"),
        ],
        Some(Path::new(workspace_path)),
        SLOW_TIMEOUT_MS,
        "PR branch update",
    )?;
    Ok(json!({
        "stdout": result.stdout,
        "stderr": result.stderr,
    }))
}
