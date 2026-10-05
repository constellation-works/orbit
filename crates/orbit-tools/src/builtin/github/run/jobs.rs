//! The runner labels of a workflow run's jobs, from the GitHub jobs API.
//!
//! `gh run view --json jobs` reports no runner, so the host CI sweep reads the
//! REST jobs listing for the labels each job asked for (`runs-on`, e.g.
//! `macos-latest` or `self-hosted, Linux, X64`). Deliberately unregistered:
//! only engine-private host automation uses it.

use std::collections::BTreeMap;

use orbit_common::OrbitError;
use orbit_exec::ExecRequest;
use serde_json::Value;

use crate::TIMEOUT_DEFAULT_MS;

/// The most jobs one listing returns; GitHub's own page ceiling.
const JOBS_PAGE_SIZE: u32 = 100;

/// `gh api repos/{owner}/{repo}/actions/runs/<run>/jobs`, newest attempt.
pub fn build_exec_request(input: &Value) -> Result<ExecRequest, OrbitError> {
    let run = super::super::require_numeric_id(input, "run")?;
    let endpoint = format!(
        "repos/{}/actions/runs/{run}/jobs",
        super::super::repository_path(input)?
    );
    let args = vec![
        "api".to_string(),
        "--method".to_string(),
        "GET".to_string(),
        endpoint,
        "-F".to_string(),
        format!("per_page={JOBS_PAGE_SIZE}"),
    ];
    Ok(super::super::gh_exec_request(
        args,
        None,
        TIMEOUT_DEFAULT_MS,
    ))
}

/// Each job's runner labels by job ID. A job reporting no labels is absent.
pub fn project_job_labels(listing: &Value) -> BTreeMap<u64, Vec<String>> {
    listing["jobs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|job| {
            let id = job["id"].as_u64()?;
            let labels: Vec<String> = job["labels"]
                .as_array()?
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|label| !label.is_empty())
                .map(ToOwned::to_owned)
                .collect();
            (!labels.is_empty()).then_some((id, labels))
        })
        .collect()
}
