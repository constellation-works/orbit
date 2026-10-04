//! Captured CI log shapes through the deterministic task-filing boundary.
#![allow(missing_docs, clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use orbit_core::OrbitRuntime;
use orbit_engine::RuntimeHost;
use orbit_tools::ToolContext;
use serde_json::{Value, json};
use tempfile::TempDir;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/ci_failure_goldens")
}

struct ChildGuard(std::process::Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn isolated(test: &str) -> bool {
    const MARKER: &str = "ORBIT_TEST_CI_LOG_GOLDEN_CHILD";
    if std::env::var_os(MARKER).is_some() {
        return true;
    }
    let home = TempDir::new().unwrap();
    let stdout = home.path().join("stdout");
    let stderr = home.path().join("stderr");
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    orbit_common::test_env::clear_inherited_authority(|key| {
        command.env_remove(key);
    });
    command
        .args(["--exact", test, "--nocapture", "--test-threads=1"])
        .env(MARKER, "1")
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .current_dir(home.path())
        .stdin(std::process::Stdio::null())
        .stdout(std::fs::File::create(&stdout).unwrap())
        .stderr(std::fs::File::create(&stderr).unwrap());
    let mut child = ChildGuard(command.spawn().unwrap());
    let deadline = Instant::now() + Duration::from_secs(120);
    let status = loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "CI fixture child exceeded 120s");
        std::thread::sleep(Duration::from_millis(20));
    };
    orbit_common::test_env::assert_child_test_passed(
        test,
        status,
        std::fs::read(stdout).unwrap(),
        std::fs::read(stderr).unwrap(),
    );
    false
}

fn failure(log: &str, index: usize, checkout: &str) -> Value {
    json!({
        "run_id": 10 + index, "job_id": 910 + index, "log_job_id": 910 + index,
        "checkout_identity": {"state": "observed", "provenance": {"job_id": 910 + index, "complete": true}},
        "workflow": "CI", "status": "completed", "conclusion": "failure", "event": "push",
        "url": format!("https://github.com/acme/orbit/actions/runs/{}", 10 + index),
        "created_at": "2026-09-07T07:24:42Z", "head_branch": "agent-main", "ref_kind": "integration",
        "event_reported_head_sha": checkout, "current_ref_head_sha": "1".repeat(40),
        "actual_checkout_shas": [checkout], "checkout_evidence": [format!("HEAD is now at {checkout}")],
        "checkout_evidence_scope": "all", "investigated": true, "log_excerpt": log,
        "log_truncated": false,
        "failed_jobs": [{"job_id": 910 + index, "name": "build", "conclusion": "failure",
            "failed_steps": [{"name": "Run CI", "conclusion": "failure"}]}]
    })
}

fn file(runtime: &OrbitRuntime, runs: Vec<Value>) -> Value {
    runtime.run_deterministic("file_ci_failure_tasks", &json!({}), &json!({"ci_evidence": {
        "schema_version": 2, "collected": true, "outcome_hint": "current_failures",
        "capability": {"available": true, "authenticated": true},
        "repository": {"name": "orbit", "full_name": "acme/orbit", "default_branch": "main"},
        "heads": [{"kind": "integration", "branch": "agent-main", "current_head_sha": "1".repeat(40)}],
        "latest_runs": runs.clone(), "current_failures": runs, "stale_or_superseded": [],
        "in_flight": [], "retryable_errors": [], "collected_at": "2026-09-07T08:00:00Z"
    }}), ToolContext::default()).expect("file CI failure")
}

#[test]
fn ci_failure_fixture_goldens() {
    if !isolated("ci_failure_fixture_goldens") {
        return;
    }
    let cases: Vec<Value> =
        serde_json::from_str(&std::fs::read_to_string(fixtures().join("fixtures.json")).unwrap())
            .unwrap();
    let update = std::env::var_os("ORBIT_UPDATE_LOG_GOLDENS").is_some();
    let expected: Value = if update {
        json!({})
    } else {
        serde_json::from_str(&std::fs::read_to_string(fixtures().join("parsed.json")).unwrap())
            .unwrap()
    };
    let mut rendered = serde_json::Map::new();
    for case in cases {
        let root = TempDir::new().unwrap();
        let global = root.path().join("home/.orbit");
        let workspace = root.path().join("repo/.orbit");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
        let mut results = Vec::new();
        let logs = case["logs"].as_array().unwrap();
        let first_batch = case["first_batch"].as_u64().unwrap_or(1) as usize;
        let split = first_batch.min(logs.len());
        let groups = std::iter::once(&logs[..split]).chain(logs[split..].chunks(1));
        let mut run_index = 0;
        for (group_index, logs) in groups.enumerate() {
            let checkout = if group_index == 0 {
                "3".repeat(40)
            } else {
                "4".repeat(40)
            };
            let runs = logs
                .iter()
                .map(|log| {
                    let run = failure(log.as_str().unwrap(), run_index, &checkout);
                    run_index += 1;
                    run
                })
                .collect();
            let output = file(&runtime, runs);
            let mut tasks = Vec::new();
            for entry in output["filed"].as_array().unwrap() {
                let task = runtime
                    .get_task(entry["task_id"].as_str().unwrap())
                    .unwrap();
                let signature = task
                    .description
                    .lines()
                    .find(|line| line.starts_with("- Normalized error signature"))
                    .unwrap();
                let section = task
                    .description
                    .split("## Failed-step log excerpt\n")
                    .nth(1)
                    .unwrap()
                    .split("\n## ")
                    .next()
                    .unwrap();
                let (excerpt, after_excerpt) = section
                    .split_once("```\n")
                    .unwrap()
                    .1
                    .split_once("\n```")
                    .unwrap();
                tasks.push(json!({
                    "failure_key": entry["failure_key"],
                    "signature": signature.split_once('`').unwrap().1.rsplit_once('`').unwrap().0,
                    "step_fallback": signature.contains("step-name fallback"),
                    "excerpt": excerpt,
                    "excerpt_has_note": after_excerpt.trim_start().starts_with('_'),
                }));
            }
            let skipped: Vec<_> = output["skipped_existing"]
                .as_array()
                .unwrap()
                .iter()
                .map(|entry| entry["failure_key"].clone())
                .collect();
            results.push(json!({"filed_count": output["filed_count"], "tasks": tasks, "skipped_keys": skipped}));
        }
        let name = case["name"].as_str().unwrap();
        let actual = json!(results);
        if !update {
            assert_eq!(
                actual, expected[name],
                "CI log fixture {name}; regenerate with make goldens UPDATE=1"
            );
        }
        rendered.insert(name.to_string(), actual);
    }
    if update {
        std::fs::write(
            fixtures().join("parsed.json"),
            serde_json::to_string_pretty(&rendered).unwrap() + "\n",
        )
        .unwrap();
    } else {
        assert_eq!(
            rendered.len(),
            expected.as_object().unwrap().len(),
            "all CI golden cases must run"
        );
    }
}

#[test]
fn ci_failure_branch_routing_retains_owner_evidence_and_only_files_landing_checkouts() {
    if !isolated(
        "ci_failure_branch_routing_retains_owner_evidence_and_only_files_landing_checkouts",
    ) {
        return;
    }
    use orbit_core::application::task::TaskAddParams;

    let root = TempDir::new().unwrap();
    let global = root.path().join("home/.orbit");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
    let owner = runtime
        .add_task(TaskAddParams {
            title: "Sandbox implementation".into(),
            ..Default::default()
        })
        .unwrap();
    let branch = format!("orbit/{}-ddb04571", owner.id);
    let mut pr = failure("error: sandbox directory escaped", 0, &"3".repeat(40));
    pr["event"] = json!("pull_request");
    pr["head_branch"] = json!(branch);
    pr["ref_kind"] = json!("pull_request");
    pr["pr_number"] = json!(3140);

    // Legacy schema-2 collectors put PR failures in current_failures too.
    let first = file(&runtime, vec![pr.clone()]);
    assert_eq!(
        first["filed_count"], 0,
        "unmerged task PRs cannot mint landing repairs"
    );
    assert_eq!(first["pilot_candidate_count"], 0);
    assert_eq!(first["attributed"][0]["task_id"], owner.id);
    let path = first["attributed"][0]["artifact"].as_str().unwrap();
    let receipt = runtime.get_task_artifact(&owner.id, path).unwrap().unwrap();
    let retained: Value = serde_json::from_slice(&receipt.content).unwrap();
    assert_eq!(retained["failure"], pr);
    assert_eq!(runtime.get_task(&owner.id).unwrap().status, owner.status);
    let repeated = file(&runtime, vec![pr.clone()]);
    assert_eq!(repeated["attributed"], first["attributed"]);
    assert_eq!(runtime.get_task_artifacts(&owner.id).unwrap().len(), 1);

    // The new collector's separate branch_failures partition uses the same route.
    let output = runtime.run_deterministic("file_ci_failure_tasks", &json!({}), &json!({
        "ci_evidence": {
            "schema_version": 2, "collected": true,
            "heads": [{"kind": "integration", "branch": "agent-main", "current_head_sha": "1".repeat(40)}],
            "current_failures": [], "branch_failures": [pr.clone()],
        }
    }), ToolContext::default()).unwrap();
    assert_eq!(output["attributed"], first["attributed"]);
    assert_eq!(output["filed_count"], 0);

    let push = failure("error: independent landing regression", 1, &"4".repeat(40));
    let output = file(&runtime, vec![pr, push.clone()]);
    assert_eq!(
        output["filed_count"], 1,
        "push failures still file landing repairs"
    );
    assert_eq!(output["attributed"].as_array().unwrap().len(), 1);
    let repeated = file(&runtime, vec![push]);
    assert_eq!(repeated["filed_count"], 0);
    assert_eq!(repeated["skipped_existing"].as_array().unwrap().len(), 1);

    // A PR source SHA equalling the tip is insufficient: its *checkout* must match.
    for (index, event, checkout, expected) in [
        (2, "pull_request", '5', 0),
        (3, "pull_request", '1', 1),
        (4, "merge_group", '1', 1),
        (5, "merge_group", '6', 0),
        (6, "push", '7', 0),
    ] {
        let mut finding = failure(
            &format!("error: routing case {index}"),
            index,
            &checkout.to_string().repeat(40),
        );
        finding["event"] = json!(event);
        finding["event_reported_head_sha"] = json!("1".repeat(40));
        if event == "merge_group" {
            finding["head_branch"] = json!("gh-readonly-queue/agent-main/pr-3140");
            finding["ref_kind"] = json!("other");
        }
        let output = file(&runtime, vec![finding]);
        assert_eq!(
            output["filed_count"].as_u64().unwrap()
                + output["skipped_existing"].as_array().unwrap().len() as u64,
            expected,
            "event {event}, checkout {checkout}"
        );
    }

    let mut orphan = failure("error: unmatched branch failure", 7, &"8".repeat(40));
    orphan["event"] = json!("pull_request");
    orphan["head_branch"] = json!("orbit/ORB-999999999-deadbeef");
    let error = runtime.run_deterministic("file_ci_failure_tasks", &json!({}), &json!({
        "ci_evidence": {"schema_version": 2, "collected": true, "current_failures": [orphan]}
    }), ToolContext::default()).unwrap_err();
    assert!(
        error.to_string().contains("task_branch_owner"),
        "missing owners remain retryable rather than minting repairs"
    );
}
