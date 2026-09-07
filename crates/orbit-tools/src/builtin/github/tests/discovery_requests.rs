//! Argv construction and response shaping for the read-only CI discovery
//! tools. These assert the contract a task body depends on without needing a
//! GitHub CLI on the machine running the tests.

use serde_json::json;

use crate::builtin::github::{dependabot_alerts, logs, pr_list, run_list, run_view};

#[test]
fn dependabot_alert_request_is_bounded_and_projects_compact_evidence() {
    let req = dependabot_alerts::build_exec_request(&json!({
        "repo": "openai/orbit",
        "limit": 500,
    }))
    .expect("request");
    assert_eq!(
        req.args,
        [
            "api",
            "--method",
            "GET",
            "repos/openai/orbit/dependabot/alerts",
            "-f",
            "state=open",
            "-F",
            "per_page=100"
        ]
    );

    let projected = dependabot_alerts::project_alert(&json!({
        "number": 7, "state": "open",
        "dependency": {"package": {"ecosystem": "cargo", "name": "time"}, "manifest_path": "Cargo.lock", "scope": "runtime"},
        "security_advisory": {"severity": "high", "ghsa_id": "GHSA-1234", "cve_id": "CVE-2026-1", "summary": "A concise summary", "description": "long prose must not escape"},
        "security_vulnerability": {"vulnerable_version_range": "< 1.2.3", "first_patched_version": {"identifier": "1.2.3"}},
        "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-02T00:00:00Z",
        "dismissed_at": null, "fixed_at": null, "html_url": "https://github.com/acme/repo/security/dependabot/7"
    }));
    assert_eq!(projected["package"], json!("time"));
    assert_eq!(projected["first_patched_version"], json!("1.2.3"));
    assert!(projected.get("description").is_none());
}

#[test]
fn dependabot_pull_request_discovery_requests_and_projects_the_head_branch() {
    let req = dependabot_alerts::build_open_pull_requests_request(&json!({
        "repo": "openai/orbit",
    }))
    .expect("request");
    assert!(
        req.args
            .windows(2)
            .any(|pair| pair == ["--json", "number,title,url,body,author,headRefName"])
    );

    let projected = dependabot_alerts::project_pull_request(&json!({
        "number": 4,
        "title": "Bump time from 1.0.0 to 1.2.3",
        "url": "https://github.test/pr/4",
        "body": "Bumps time from 1.0.0 to 1.2.3.",
        "author": {"login": "app/dependabot"},
        "headRefName": "dependabot/cargo/time-1.2.3",
    }));
    assert_eq!(
        projected["head_branch"],
        json!("dependabot/cargo/time-1.2.3")
    );
    assert_eq!(projected["author"], json!("app/dependabot"));
}

#[test]
fn dependabot_alert_request_rejects_repository_path_injection() {
    let error = dependabot_alerts::build_exec_request(&json!({"repo": "acme/repo?per_page=999"}))
        .expect_err("invalid repo must fail");
    assert!(error.to_string().contains("invalid `repo`"));
}

#[test]
fn code_and_secret_scanning_requests_are_bounded_and_validate_repository() {
    for (request, endpoint) in [
        (
            dependabot_alerts::build_code_scanning_request(&json!({
                "repo": "openai/orbit", "limit": 500
            }))
            .expect("code scanning request"),
            "repos/openai/orbit/code-scanning/alerts",
        ),
        (
            dependabot_alerts::build_secret_scanning_request(&json!({
                "repo": "openai/orbit", "limit": 500
            }))
            .expect("secret scanning request"),
            "repos/openai/orbit/secret-scanning/alerts",
        ),
    ] {
        assert_eq!(request.args[3], endpoint);
        assert_eq!(
            request.args.last().map(String::as_str),
            Some("per_page=100")
        );
        assert!(
            request
                .args
                .windows(2)
                .any(|pair| pair == ["-f", "state=open"])
        );
    }

    let error = dependabot_alerts::build_secret_scanning_request(
        &json!({"repo": "acme/repo?state=resolved"}),
    )
    .expect_err("invalid repo must fail");
    assert!(error.to_string().contains("invalid `repo`"));
}

#[test]
fn scanning_projections_retain_evidence_but_structurally_drop_secret() {
    let code = dependabot_alerts::project_code_scanning_alert(&json!({
        "number": 9,
        "state": "open",
        "rule": {"id": "rust/sql-injection", "name": "SQL injection", "description": "unsafe query", "security_severity_level": "high", "tags": ["security"]},
        "tool": {"name": "CodeQL", "guid": "tool-guid", "version": "2.0"},
        "most_recent_instance": {"ref": "refs/heads/main", "commit_sha": "abc123", "message": {"text": "user input reaches query"}, "location": {"path": "src/db.rs", "start_line": 17, "end_line": 19, "start_column": 4, "end_column": 20}, "classifications": ["test"]},
        "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-02T00:00:00Z", "html_url": "https://github.test/code/9",
        "instances_url": "must not escape"
    }));
    assert_eq!(code["rule_id"], "rust/sql-injection");
    assert_eq!(code["path"], "src/db.rs");
    assert!(code.get("instances_url").is_none());

    let sentinel = "orbit-sentinel-secret-99f0";
    let secret = dependabot_alerts::project_secret_scanning_alert(&json!({
        "number": 12, "state": "open", "secret_type": "example_token",
        "secret_type_display_name": "Example token", "secret": sentinel,
        "validity": "active", "publicly_leaked": false, "multi_repo": false,
        "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-02T00:00:00Z",
        "html_url": "https://github.test/secret/12"
    }));
    assert_eq!(secret["secret_type"], "example_token");
    assert!(
        !serde_json::to_string(&secret)
            .expect("encode")
            .contains(sentinel)
    );
    assert!(secret.get("secret").is_none());

    let location = dependabot_alerts::project_secret_location(&json!({
        "type": "commit", "details": {"path": "config/dev.env", "start_line": 3,
        "end_line": 3, "commit_sha": "def456", "commit_url": "https://github.test/commit/def456",
        "diff": sentinel}
    }));
    assert_eq!(location["path"], "config/dev.env");
    assert!(
        !serde_json::to_string(&location)
            .expect("encode")
            .contains(sentinel)
    );
}

#[test]
fn run_list_applies_every_filter_and_caps_the_limit() {
    let req = run_list::build_exec_request(&json!({
        "branch": "release",
        "workflow": "ci.yml",
        "status": "failure",
        "event": "push",
        "limit": 5_000,
        "repo": "owner/name",
    }))
    .expect("request");

    assert_eq!(req.program, "gh");
    assert_eq!(&req.args[..2], &["run".to_string(), "list".to_string()]);
    for expected in [
        "--branch",
        "release",
        "--workflow",
        "ci.yml",
        "--status",
        "failure",
        "--event",
        "push",
        "--repo",
        "owner/name",
    ] {
        assert!(
            req.args.iter().any(|arg| arg == expected),
            "missing {expected} in {:?}",
            req.args
        );
    }
    let limit = req
        .args
        .iter()
        .position(|arg| arg == "--limit")
        .map(|index| req.args[index + 1].clone())
        .expect("limit flag");
    assert_eq!(
        limit, "100",
        "an oversized limit must clamp, not pass through"
    );
}

#[test]
fn run_list_defaults_to_an_unfiltered_bounded_query() {
    let req = run_list::build_exec_request(&json!({})).expect("request");

    assert!(!req.args.iter().any(|arg| arg == "--branch"));
    let limit = req
        .args
        .iter()
        .position(|arg| arg == "--limit")
        .map(|index| req.args[index + 1].clone())
        .expect("limit flag");
    assert_eq!(limit, "20");
}

#[test]
fn a_filter_value_cannot_smuggle_another_gh_flag() {
    let error = run_list::build_exec_request(&json!({ "branch": "--repo" }))
        .expect_err("a flag-shaped filter must be rejected");

    assert!(error.to_string().contains("branch"), "{error}");
}

#[test]
fn run_view_requires_a_numeric_run_id() {
    let error = run_view::build_exec_request(&json!({ "run": "--log" }))
        .expect_err("a non-numeric run must be rejected");

    assert!(error.to_string().contains("run"), "{error}");
}

#[test]
fn run_view_projects_the_reported_head_sha_and_collects_failed_jobs() {
    let projected = run_view::project_run_view(&json!({
        "databaseId": 42,
        "number": 7,
        "workflowName": "CI",
        "headSha": "abc123",
        "event": "pull_request",
        "url": "https://example.invalid/o/r/actions/runs/42",
        "jobs": [
            {
                "databaseId": 1,
                "name": "green",
                "conclusion": "success",
                "steps": [{"number": 1, "name": "run", "conclusion": "success"}],
            },
            {
                "databaseId": 2,
                "name": "red",
                "conclusion": "failure",
                "steps": [
                    {"number": 1, "name": "checkout", "conclusion": "success"},
                    {"number": 2, "name": "test", "conclusion": "failure"},
                ],
            },
        ],
    }));

    assert_eq!(projected["reported_head_sha"], "abc123");
    assert!(
        projected.get("sha").is_none(),
        "the reported head SHA must not also appear under an ambiguous name"
    );
    assert_eq!(projected["jobs"].as_array().expect("jobs").len(), 2);
    let failed = projected["failed_jobs"].as_array().expect("failed jobs");
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0]["name"], "red");
    assert_eq!(
        failed[0]["failed_steps"].as_array().expect("steps").len(),
        1
    );
    assert_eq!(failed[0]["failed_steps"][0]["name"], "test");
    assert_eq!(
        failed[0]["url"], "https://example.invalid/o/r/actions/runs/42/job/2",
        "a failing job must be linkable even though gh reports no job URL"
    );
}

#[test]
fn a_cancelled_job_counts_as_unsuccessful() {
    let projected = run_view::project_run_view(&json!({
        "jobs": [{"databaseId": 1, "name": "flaky", "conclusion": "cancelled", "steps": []}],
    }));

    assert_eq!(projected["failed_jobs"].as_array().expect("jobs").len(), 1);
}

#[test]
fn run_logs_defaults_to_failed_steps_and_accepts_the_full_log() {
    let failed = logs::run_log_request(&json!({ "run": "99" })).expect("request");
    assert!(failed.args.iter().any(|arg| arg == "--log-failed"));
    assert!(!failed.args.iter().any(|arg| arg == "--log"));

    let all = logs::run_log_request(&json!({ "run": "99", "scope": "all", "job": "5" }))
        .expect("request");
    assert!(all.args.iter().any(|arg| arg == "--log"));
    assert!(all.args.iter().any(|arg| arg == "--job"));
    assert!(all.args.iter().any(|arg| arg == "5"));
}

#[test]
fn run_logs_rejects_an_unknown_scope() {
    let error = logs::run_log_request(&json!({ "run": "99", "scope": "everything" }))
        .expect_err("an unknown scope must be rejected");

    assert!(error.to_string().contains("scope"), "{error}");
}

#[test]
fn pr_list_projects_each_head_sha_under_its_own_name() {
    let projected = pr_list::project_pull_request(&json!({
        "number": 3,
        "title": "Fix it",
        "state": "OPEN",
        "isDraft": false,
        "headRefName": "topic",
        "headRefOid": "deadbee",
        "baseRefName": "trunk",
    }));

    assert_eq!(projected["reported_head_sha"], "deadbee");
    assert_eq!(projected["head_branch"], "topic");
    assert!(projected.get("sha").is_none());
}

#[test]
fn pr_list_defaults_to_a_bounded_open_query() {
    let req = pr_list::build_exec_request(&json!({})).expect("request");

    assert_eq!(&req.args[..2], &["pr".to_string(), "list".to_string()]);
    let limit = req
        .args
        .iter()
        .position(|arg| arg == "--limit")
        .map(|index| req.args[index + 1].clone())
        .expect("limit flag");
    assert_eq!(limit, "30");
}
