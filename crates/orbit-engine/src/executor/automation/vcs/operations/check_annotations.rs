//! [ORB-16185] The failure annotations of one required check run that no
//! workflow produced, such as a provider app's aggregate result.
//!
//! Every read goes to the provider's own endpoints for the checkout's
//! repository; the caller supplies only the identities the head-bound
//! required-check read reported. The answer is refused, never narrowed, when
//! it names another repository, pull request, head, app or check run, when
//! more than one latest run carries the check's name, when a listing or the
//! annotation pages are incomplete, or when the run or the pull request
//! changed while it was read. Annotation contents are returned as provider
//! data; the engine's reader decides whether they name source diagnostics.

use std::path::Path;

use orbit_common::OrbitError;
use serde_json::{Value, json};

use super::DEFAULT_TIMEOUT_MS;
use super::input::{number, required_string, same, text, valid_expected_remote_sha};
use super::process::execute_with_transient_retry;

/// The provider's annotation page size.
const PAGE: u64 = 100;
/// Annotation pages one read follows before it refuses as too large.
const MAX_PAGES: u64 = 10;

fn refused(reason: &str) -> OrbitError {
    OrbitError::Execution(format!(
        "required check annotations are unavailable ({reason})"
    ))
}

/// Read one required check run's annotations on a pull request's head.
pub(super) fn ci_check_annotations(input: &Value) -> Result<Value, OrbitError> {
    let workspace = Path::new(required_string(input, "workspace_path")?);
    let pr = required_string(input, "pr")?;
    let head = required_string(input, "head_sha")?;
    let name = required_string(input, "name")?;
    let check_id = number(input, "check_run_id", refused)?;
    let app_id = number(input, "app_id", refused)?;
    if !pr.bytes().all(|byte| byte.is_ascii_digit()) || !valid_expected_remote_sha(Some(head)) {
        return Err(OrbitError::InvalidInput(
            "private automation VCS check annotations require a PR number and a full head id"
                .to_string(),
        ));
    }
    let pr_number: u64 = pr.parse().map_err(|_| refused("PR number"))?;
    let deadline = input
        .get("deadline_ms")
        .map(|_| super::super::completion_deadline::Deadline::from_input(input))
        .transpose()?;
    let read = |endpoint: String| -> Result<Value, OrbitError> {
        let args = vec!["api".to_string(), endpoint];
        let result = match deadline {
            Some(deadline) => deadline.execute("gh", args, workspace, "check annotations")?,
            None => execute_with_transient_retry(
                "gh",
                &args,
                Some(workspace),
                DEFAULT_TIMEOUT_MS,
                "check annotations",
            )?,
        };
        serde_json::from_str(&result.stdout).map_err(|_| refused("invalid API JSON"))
    };

    let repo = read("repos/{owner}/{repo}".into())?;
    let repository = text(&repo, "full_name", refused)?.to_string();
    let repository_id = number(&repo, "id", refused)?;
    let api_url = text(&repo, "url", refused)?.to_string();
    let pull_bound = |pull: &Value| -> Result<String, OrbitError> {
        same(&pull["number"], &json!(pr_number), "PR", refused)?;
        same(&pull["state"], &json!("open"), "PR state", refused)?;
        same(&pull["head"]["sha"], &json!(head), "PR head", refused)?;
        for side in ["head", "base"] {
            same(
                &pull[side]["repo"]["id"],
                &json!(repository_id),
                "PR repository",
                refused,
            )?;
        }
        Ok(text(&pull["base"], "ref", refused)?.to_string())
    };
    let pull = read(format!("repos/{{owner}}/{{repo}}/pulls/{pr}"))?;
    let base_ref = pull_bound(&pull)?;

    let listing = read(format!(
        "repos/{{owner}}/{{repo}}/commits/{head}/check-runs?check_name={}&filter=latest&per_page=100",
        super::prerequisite::encoded(name)
    ))?;
    let runs = listing["check_runs"]
        .as_array()
        .ok_or_else(|| refused("check run listing"))?;
    if listing["total_count"].as_u64() != Some(runs.len() as u64) || runs.len() as u64 >= PAGE {
        return Err(refused("truncated check run listing"));
    }
    let [run] = runs.as_slice() else {
        return Err(refused(&format!(
            "{} latest check runs named '{name}' on the head",
            runs.len()
        )));
    };
    same(&run["name"], &json!(name), "check name", refused)?;
    same(&run["id"], &json!(check_id), "check run", refused)?;
    same(&run["app"]["id"], &json!(app_id), "check app", refused)?;
    same(&run["head_sha"], &json!(head), "check head", refused)?;
    same(
        &run["url"],
        &json!(format!("{api_url}/check-runs/{check_id}")),
        "check repository",
        refused,
    )?;
    same(&run["status"], &json!("completed"), "check status", refused)?;
    same(
        &run["conclusion"],
        &json!("failure"),
        "check conclusion",
        refused,
    )?;
    let linked = run["pull_requests"]
        .as_array()
        .ok_or_else(|| refused("check PR linkage"))?
        .iter()
        .filter(|linked| linked["number"] == json!(pr_number))
        .collect::<Vec<_>>();
    let [linked] = linked.as_slice() else {
        return Err(refused("check PR linkage"));
    };
    same(
        &linked["head"]["sha"],
        &json!(head),
        "check PR head",
        refused,
    )?;
    same(
        &linked["base"]["ref"],
        &json!(base_ref),
        "check PR base",
        refused,
    )?;
    same(
        &linked["base"]["repo"]["id"],
        &json!(repository_id),
        "check PR repository",
        refused,
    )?;

    let count = run["output"]["annotations_count"]
        .as_u64()
        .ok_or_else(|| refused("annotation count"))?;
    if count > PAGE * MAX_PAGES {
        return Err(refused("too many annotations to read in full"));
    }
    let mut annotations = Vec::new();
    for page in 1..=count.div_ceil(PAGE) {
        let listed = read(format!(
            "repos/{{owner}}/{{repo}}/check-runs/{check_id}/annotations?per_page={PAGE}&page={page}"
        ))?;
        let listed = listed
            .as_array()
            .ok_or_else(|| refused("annotation page"))?;
        let expected = (count - (page - 1) * PAGE).min(PAGE);
        if listed.len() as u64 != expected {
            return Err(refused("incomplete annotation pages"));
        }
        annotations.extend(listed.iter().cloned());
    }

    // The run and the pull request must still be what was read.
    let again = read(format!("repos/{{owner}}/{{repo}}/check-runs/{check_id}"))?;
    for pointer in [
        "/id",
        "/name",
        "/head_sha",
        "/status",
        "/conclusion",
        "/completed_at",
        "/app/id",
        "/check_suite/id",
        "/output/annotations_count",
    ] {
        same(
            &again.pointer(pointer).cloned().unwrap_or(Value::Null),
            &run.pointer(pointer).cloned().unwrap_or(Value::Null),
            &format!("check run {pointer}"),
            refused,
        )?;
    }
    let pull_again = read(format!("repos/{{owner}}/{{repo}}/pulls/{pr}"))?;
    if pull_bound(&pull_again)? != base_ref {
        return Err(refused("foreign or changed PR base"));
    }

    Ok(json!({
        "repository": repository,
        "repository_id": repository_id,
        "pr": pr_number,
        "head_sha": head,
        "base_ref": base_ref,
        "check_run": {
            "id": check_id,
            "name": name,
            "app_id": app_id,
            "app_slug": run["app"]["slug"],
            "status": run["status"],
            "conclusion": run["conclusion"],
            "completed_at": run["completed_at"],
            "check_suite_id": run["check_suite"]["id"],
            "html_url": run["html_url"],
            "title": run["output"]["title"],
            "annotations_count": count,
        },
        "annotations": annotations,
    }))
}
