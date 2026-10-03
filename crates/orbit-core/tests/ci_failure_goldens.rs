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

fn isolated() -> bool {
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
        .args([
            "--exact",
            "ci_failure_fixture_goldens",
            "--nocapture",
            "--test-threads=1",
        ])
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
    assert!(
        status.success(),
        "CI fixture child failed:\n{}\n{}",
        std::fs::read_to_string(stdout).unwrap(),
        std::fs::read_to_string(stderr).unwrap()
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
        "event_reported_head_sha": "1".repeat(40), "current_ref_head_sha": "1".repeat(40),
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
    if !isolated() {
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
