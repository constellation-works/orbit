use orbit_common::OrbitError;
use orbit_exec::ExecRequest;
use serde_json::{Value, json};

use crate::TIMEOUT_DEFAULT_MS;

const DEFAULT_LIMIT: u64 = 100;
const MAX_LIMIT: u64 = 100;

/// Build one bounded Dependabot-alert API request. This builder is deliberately
/// unregistered: only engine-private host automation calls it.
pub fn build_exec_request(input: &Value) -> Result<ExecRequest, OrbitError> {
    build_alert_request(input, "dependabot")
}

/// Build one bounded Code scanning alert request. Deliberately unregistered:
/// this is only available to engine-private host automation.
pub fn build_code_scanning_request(input: &Value) -> Result<ExecRequest, OrbitError> {
    build_alert_request(input, "code-scanning")
}

/// Build one bounded secret scanning alert request. Deliberately unregistered:
/// this is only available to engine-private host automation.
pub fn build_secret_scanning_request(input: &Value) -> Result<ExecRequest, OrbitError> {
    build_alert_request(input, "secret-scanning")
}

fn build_alert_request(input: &Value, family: &str) -> Result<ExecRequest, OrbitError> {
    let endpoint = format!("repos/{}/{family}/alerts", super::repository_path(input)?);
    let limit = super::bounded_limit(input, "limit", DEFAULT_LIMIT, MAX_LIMIT)?;
    let args = vec![
        "api".to_string(),
        "--method".to_string(),
        "GET".to_string(),
        endpoint,
        "-f".to_string(),
        "state=open".to_string(),
        "-F".to_string(),
        format!("per_page={limit}"),
    ];
    Ok(super::gh_exec_request(args, None, TIMEOUT_DEFAULT_MS))
}

/// Build a bounded request for the non-secret locations attached to one
/// secret-scanning alert. The response is projected before it leaves the host.
pub fn build_secret_locations_request(input: &Value) -> Result<ExecRequest, OrbitError> {
    let number = input
        .get("alert_number")
        .and_then(Value::as_u64)
        .filter(|number| *number > 0)
        .ok_or_else(|| {
            OrbitError::InvalidInput("input.alert_number must be a positive integer".to_string())
        })?;
    let limit = super::bounded_limit(input, "limit", DEFAULT_LIMIT, MAX_LIMIT)?;
    let endpoint = format!(
        "repos/{}/secret-scanning/alerts/{number}/locations",
        super::repository_path(input)?
    );
    Ok(super::gh_exec_request(
        vec![
            "api".to_string(),
            "--method".to_string(),
            "GET".to_string(),
            endpoint,
            "-F".to_string(),
            format!("per_page={limit}"),
        ],
        None,
        TIMEOUT_DEFAULT_MS,
    ))
}

/// Build the bounded query used to discover Dependabot-authored open PRs.
pub fn build_open_pull_requests_request(input: &Value) -> Result<ExecRequest, OrbitError> {
    let limit = super::bounded_limit(input, "limit", DEFAULT_LIMIT, MAX_LIMIT)?;
    let mut args = vec![
        "pr".to_string(),
        "list".to_string(),
        "--state".to_string(),
        "open".to_string(),
        "--author".to_string(),
        "app/dependabot".to_string(),
    ];
    super::push_repo_flag(&mut args, input)?;
    args.extend([
        "--limit".to_string(),
        limit.to_string(),
        "--json".to_string(),
        "number,title,url,body,author,headRefName".to_string(),
    ]);
    Ok(super::gh_exec_request(args, None, TIMEOUT_DEFAULT_MS))
}

pub fn project_alert(alert: &Value) -> Value {
    json!({
        "number": alert["number"],
        "state": alert["state"],
        "ecosystem": alert["dependency"]["package"]["ecosystem"],
        "package": alert["dependency"]["package"]["name"],
        "manifest_path": alert["dependency"]["manifest_path"],
        "scope": alert["dependency"]["scope"],
        "severity": alert["security_advisory"]["severity"],
        "ghsa_id": alert["security_advisory"]["ghsa_id"],
        "cve_id": alert["security_advisory"]["cve_id"],
        "summary": alert["security_advisory"]["summary"],
        "vulnerable_version_range": alert["security_vulnerability"]["vulnerable_version_range"],
        "first_patched_version": alert["security_vulnerability"]["first_patched_version"]["identifier"],
        "created_at": alert["created_at"],
        "updated_at": alert["updated_at"],
        "dismissed_at": alert["dismissed_at"],
        "fixed_at": alert["fixed_at"],
        "html_url": alert["html_url"],
    })
}

pub fn project_pull_request(pr: &Value) -> Value {
    json!({
        "number": pr["number"],
        "title": pr["title"],
        "url": pr["url"],
        "body": pr["body"],
        "author": pr["author"]["login"],
        "head_branch": pr["headRefName"],
    })
}

/// Project only remediation evidence from a Code scanning alert.
pub fn project_code_scanning_alert(alert: &Value) -> Value {
    json!({
        "number": alert["number"],
        "state": alert["state"],
        "rule_id": alert["rule"]["id"],
        "rule_name": alert["rule"]["name"],
        "rule_description": alert["rule"]["description"],
        "security_severity": alert["rule"]["security_severity_level"],
        "tool_name": alert["tool"]["name"],
        "tool_guid": alert["tool"]["guid"],
        "tool_version": alert["tool"]["version"],
        "message": alert["most_recent_instance"]["message"]["text"],
        "ref": alert["most_recent_instance"]["ref"],
        "commit_sha": alert["most_recent_instance"]["commit_sha"],
        "path": alert["most_recent_instance"]["location"]["path"],
        "start_line": alert["most_recent_instance"]["location"]["start_line"],
        "end_line": alert["most_recent_instance"]["location"]["end_line"],
        "start_column": alert["most_recent_instance"]["location"]["start_column"],
        "end_column": alert["most_recent_instance"]["location"]["end_column"],
        "created_at": alert["created_at"],
        "updated_at": alert["updated_at"],
        "html_url": alert["html_url"],
    })
}

/// Project a secret-scanning alert without ever copying GitHub's `secret`
/// member. This structural omission is the primary credential boundary;
/// generic redaction remains a later defense in depth.
pub fn project_secret_scanning_alert(alert: &Value) -> Value {
    json!({
        "number": alert["number"],
        "state": alert["state"],
        "secret_type": alert["secret_type"],
        "secret_type_display_name": alert["secret_type_display_name"],
        "validity": alert["validity"],
        "publicly_leaked": alert["publicly_leaked"],
        "multi_repo": alert["multi_repo"],
        "created_at": alert["created_at"],
        "updated_at": alert["updated_at"],
        "html_url": alert["html_url"],
    })
}

/// Project only documented, non-secret location metadata. In particular, no
/// diff, snippet, or raw credential-shaped response member is retained.
pub fn project_secret_location(location: &Value) -> Value {
    let details = &location["details"];
    json!({
        "type": location["type"],
        "path": details["path"],
        "start_line": details["start_line"],
        "end_line": details["end_line"],
        "start_column": details["start_column"],
        "end_column": details["end_column"],
        "blob_sha": details["blob_sha"],
        "blob_url": details["blob_url"],
        "commit_sha": details["commit_sha"],
        "commit_url": details["commit_url"],
        "issue_title": details["issue_title"],
        "issue_url": details["issue_url"],
        "pull_request_title": details["pull_request_title"],
        "pull_request_url": details["pull_request_url"],
        "discussion_title": details["discussion_title"],
        "discussion_url": details["discussion_url"],
        "wiki_commit_sha": details["wiki_commit_sha"],
        "user_login": details["user_login"],
    })
}
