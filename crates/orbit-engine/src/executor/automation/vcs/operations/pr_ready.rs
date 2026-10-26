//! Private provider mutation for guarded draft recovery.

use super::SLOW_TIMEOUT_MS;
use super::input::required_string;
use super::process::execute;
use orbit_common::OrbitError;
use serde_json::{Value, json};
use std::path::Path;

pub(super) fn mark_pr_ready(input: &Value) -> Result<Value, OrbitError> {
    let pr = required_string(input, "pr")?;
    let workspace = Path::new(required_string(input, "workspace_path")?);
    if pr.is_empty() || !pr.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(OrbitError::InvalidInput(
            "draft recovery requires a PR number".into(),
        ));
    }
    let result = execute(
        "gh",
        vec!["pr".into(), "ready".into(), pr.into()],
        Some(workspace),
        SLOW_TIMEOUT_MS,
        "PR ready",
    )?;
    Ok(json!({"stdout": result.stdout, "stderr": result.stderr}))
}
