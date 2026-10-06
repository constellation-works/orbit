//! The check-run annotations of one workflow job, from the GitHub REST API.
//!
//! A job is a check run with the same numeric ID, and GitHub records why it
//! cancelled a job there rather than in `gh run view` or the jobs listing. A
//! workflow concurrency group that cancels an older run in favour of a newer
//! one leaves the annotation "Canceling since a higher priority waiting request
//! for '<group>' exists". Deliberately unregistered: only engine-private host
//! automation uses it.

use orbit_common::OrbitError;
use orbit_exec::ExecRequest;
use serde_json::Value;

use crate::TIMEOUT_DEFAULT_MS;

/// A job carries a handful of annotations; one page is ample.
const ANNOTATIONS_PAGE_SIZE: u32 = 50;

/// The message GitHub writes when a concurrency group cancels a run.
const CONCURRENCY_CANCELLATION_PREFIX: &str = "Canceling since a higher priority waiting request";

/// `gh api repos/{owner}/{repo}/check-runs/<job>/annotations`.
pub fn build_exec_request(input: &Value) -> Result<ExecRequest, OrbitError> {
    let job = super::super::require_numeric_id(input, "job")?;
    let endpoint = format!(
        "repos/{}/check-runs/{job}/annotations",
        super::super::repository_path(input)?
    );
    let args = vec![
        "api".to_string(),
        "--method".to_string(),
        "GET".to_string(),
        endpoint,
        "-F".to_string(),
        format!("per_page={ANNOTATIONS_PAGE_SIZE}"),
    ];
    Ok(super::super::gh_exec_request(
        args,
        None,
        TIMEOUT_DEFAULT_MS,
    ))
}

/// The concurrency-cancellation annotation in one job's listing, if any.
pub fn concurrency_cancellation(listing: &Value) -> Option<String> {
    listing
        .as_array()?
        .iter()
        .filter_map(|annotation| annotation["message"].as_str())
        .map(str::trim)
        .find(|message| message.starts_with(CONCURRENCY_CANCELLATION_PREFIX))
        .map(ToOwned::to_owned)
}
