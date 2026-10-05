use orbit_common::OrbitError;
use orbit_exec::ExecRequest;
use serde_json::{Value, json};

use crate::TIMEOUT_DEFAULT_MS;

pub fn build_exec_request(input: &Value) -> Result<ExecRequest, OrbitError> {
    let mut args = vec!["repo".to_string(), "view".to_string()];

    if let Some(repo) = input.get("repo").and_then(Value::as_str) {
        args.push("--repo".to_string());
        args.push(repo.to_string());
    }

    args.push("--json".to_string());
    args.push("name,nameWithOwner,defaultBranchRef".to_string());

    Ok(super::gh_exec_request(args, None, TIMEOUT_DEFAULT_MS))
}

/// Reshape `gh repo view --json` into this surface's own field names.
///
/// `default_branch` is the repository's *release* branch as GitHub itself
/// reports it. Deriving it here is the reason nothing downstream has to guess
/// a branch name from convention.
pub fn project_repo_view(parsed: &Value) -> Value {
    json!({
        "name": parsed["name"],
        "full_name": parsed["nameWithOwner"],
        "default_branch": parsed["defaultBranchRef"]["name"],
    })
}
