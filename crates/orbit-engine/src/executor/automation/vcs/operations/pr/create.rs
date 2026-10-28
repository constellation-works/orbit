use std::path::Path;

use orbit_common::OrbitError;
use serde_json::{Value, json};

use super::super::SLOW_TIMEOUT_MS;
use super::super::input::required_string;
use super::super::process::execute;

/// One create attempt. `pr_open` owns retries so it can reconcile an ambiguous
/// response with a head lookup before sending another mutation.
pub(in crate::executor::automation::vcs::operations) fn pr_create(
    input: &Value,
) -> Result<Value, OrbitError> {
    let title = required_string(input, "title")?;
    let body = required_string(input, "body")?;
    let base = required_string(input, "base")?;
    let head = required_string(input, "head")?;
    let workspace_path = required_string(input, "workspace_path")?;
    let mut args = vec![
        "pr".to_string(),
        "create".to_string(),
        "--title".to_string(),
        title.to_string(),
        "--body".to_string(),
        body.to_string(),
        "--base".to_string(),
        base.to_string(),
        "--head".to_string(),
        head.to_string(),
    ];
    // `pr.draft`, `pr.labels` and `pr.reviewers`; absent fields add nothing.
    if input.get("draft").and_then(Value::as_bool) == Some(true) {
        args.push("--draft".to_string());
    }
    for (field, flag) in [("labels", "--label"), ("reviewers", "--reviewer")] {
        for value in optional_string_list(input, field)? {
            args.push(flag.to_string());
            args.push(value);
        }
    }
    let result = execute(
        "gh",
        args,
        Some(Path::new(workspace_path)),
        SLOW_TIMEOUT_MS,
        "PR create",
    )?;
    Ok(json!({
        "url": result.stdout.trim(),
        "stdout": result.stdout,
        "stderr": result.stderr,
    }))
}

/// A list of non-empty strings at `field`; absent or null is empty. A value
/// that could be read as a flag is refused, since it is passed to `gh`.
fn optional_string_list(input: &Value, field: &str) -> Result<Vec<String>, OrbitError> {
    let Some(values) = input.get(field).filter(|value| !value.is_null()) else {
        return Ok(Vec::new());
    };
    let invalid = || {
        OrbitError::InvalidInput(format!(
            "private automation VCS PR create input.{field} must be an array of non-empty \
             strings that do not start with '-'"
        ))
    };
    values
        .as_array()
        .ok_or_else(invalid)?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty() && !value.starts_with('-'))
                .map(ToOwned::to_owned)
                .ok_or_else(invalid)
        })
        .collect()
}
