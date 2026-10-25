//! Best-effort, idempotent task-derived labels, separate from PR creation.

use std::collections::BTreeSet;
use std::path::Path;

use orbit_common::OrbitError;
use serde_json::{Value, json};

use super::DEFAULT_TIMEOUT_MS;
use super::input::{reject_option_like, required_string};
use super::process::execute_with_transient_retry;

pub(super) fn add_labels(input: &Value) -> Result<Value, OrbitError> {
    let workspace_path = Path::new(required_string(input, "workspace_path")?);
    let pr = required_string(input, "pr")?;
    if !pr.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(OrbitError::InvalidInput(
            "private automation VCS label operation requires a PR number".to_string(),
        ));
    }
    let values = input
        .get("labels")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            OrbitError::InvalidInput("private automation VCS labels must be an array".to_string())
        })?;
    let mut labels = BTreeSet::new();
    for value in values {
        let label = value
            .as_str()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                OrbitError::InvalidInput(
                    "private automation VCS labels must be non-empty strings".to_string(),
                )
            })?;
        reject_option_like("label", label.trim())?;
        labels.insert(label.to_string());
    }

    let mut ready = Vec::new();
    let mut failed = Vec::new();
    for label in labels {
        let args = vec!["label".into(), "create".into(), label.clone()];
        match execute_with_transient_retry(
            "gh",
            &args,
            Some(workspace_path),
            DEFAULT_TIMEOUT_MS,
            "label create",
        ) {
            Ok(_) => ready.push(label),
            // No --force: an existing label's description and color belong to
            // the repository. A retry after an ambiguous create is safe too.
            Err(error)
                if error
                    .to_string()
                    .to_ascii_lowercase()
                    .contains("already exists") =>
            {
                ready.push(label);
            }
            Err(error) => {
                tracing::warn!(%label, %error, "could not create task-derived PR label");
                failed.push(json!({"label": label, "reason": error.to_string()}));
            }
        }
    }
    let mut applied = Vec::new();
    if !ready.is_empty() {
        let mut args = vec!["pr".into(), "edit".into(), pr.into()];
        for label in &ready {
            args.push("--add-label".into());
            // gh parses this flag as CSV; quote names with commas or quotes
            // so one configured label cannot expand into unrequested labels.
            args.push(if label.contains([',', '"']) {
                format!("\"{}\"", label.replace('"', "\"\""))
            } else {
                label.clone()
            });
        }
        match execute_with_transient_retry(
            "gh",
            &args,
            Some(workspace_path),
            DEFAULT_TIMEOUT_MS,
            "PR add labels",
        ) {
            Ok(_) => applied = ready,
            Err(error) => {
                tracing::warn!(%pr, %error, "could not apply task-derived PR labels");
                failed.extend(
                    ready
                        .into_iter()
                        .map(|label| json!({"label": label, "reason": error.to_string()})),
                );
            }
        }
    }
    Ok(json!({"applied": applied, "failed": failed}))
}
