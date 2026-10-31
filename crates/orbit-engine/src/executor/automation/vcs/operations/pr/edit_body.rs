//! Private provider mutation for guarded draft recovery.

use std::path::Path;

use orbit_common::OrbitError;
use serde_json::{Value, json};

use super::super::SLOW_TIMEOUT_MS;
use super::super::input::required_string;
use super::super::process::execute;

pub(in crate::executor::automation::vcs::operations) fn edit_pr_body(
    input: &Value,
) -> Result<Value, OrbitError> {
    let pr = required_string(input, "pr")?;
    let workspace = Path::new(required_string(input, "workspace_path")?);
    if pr.is_empty() || !pr.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(OrbitError::InvalidInput(
            "draft recovery requires a PR number".into(),
        ));
    }
    let result = execute(
        "gh",
        vec![
            "pr".into(),
            "edit".into(),
            pr.into(),
            "--body".into(),
            required_string(input, "body")?.into(),
        ],
        Some(workspace),
        SLOW_TIMEOUT_MS,
        "PR edit body",
    )?;
    Ok(json!({"stdout": result.stdout, "stderr": result.stderr}))
}
