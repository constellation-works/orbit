#![allow(missing_docs)]

use std::collections::HashMap;

use serde_json::json;

use crate::template::TemplateContext;

use super::super::input::required_job_run_id;

#[test]
fn required_job_run_id_prefers_job_run_id_over_run_id_and_batch_id() {
    let input = json!({
        "job_run_id": "job-run",
        "run_id": "run",
        "batch_id": "batch",
    });

    assert_eq!(required_job_run_id(&input, "pr_open").unwrap(), "job-run");
}

#[test]
fn required_job_run_id_falls_back_to_run_id_before_batch_id() {
    let input = json!({
        "job_run_id": "",
        "run_id": "run",
        "batch_id": "batch",
    });

    assert_eq!(required_job_run_id(&input, "pr_open").unwrap(), "run");
}

#[test]
fn required_job_run_id_accepts_legacy_batch_id() {
    let input = json!({
        "batch_id": "legacy-batch",
    });

    assert_eq!(
        required_job_run_id(&input, "pr_open").unwrap(),
        "legacy-batch"
    );
}

#[test]
fn required_job_run_id_names_batch_id_in_missing_key_error() {
    let error = required_job_run_id(&json!({}), "pr_open").unwrap_err();

    assert!(
        error
            .to_string()
            .contains("requires input.job_run_id, input.run_id, or input.batch_id")
    );
}

#[test]
fn legacy_batch_id_template_output_resolves_as_job_run_id() {
    let mut steps = HashMap::new();
    steps.insert(
        "worktree".to_string(),
        json!({
            "output": {
                "job_run_id": "jrun-legacy",
                "batch_id": "jrun-legacy",
            }
        }),
    );
    let context = TemplateContext {
        steps: std::sync::Arc::new(steps),
        ..TemplateContext::default()
    };
    let batch_id =
        crate::template::render("{{ steps.worktree.output.batch_id }}", &context).unwrap();
    let input = json!({
        "batch_id": batch_id,
    });

    assert_eq!(
        required_job_run_id(&input, "pr_open").unwrap(),
        "jrun-legacy"
    );
}
