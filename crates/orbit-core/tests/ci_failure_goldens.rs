//! Captured CI log shapes through the deterministic task-filing boundary.
#![allow(missing_docs, clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use orbit_core::OrbitRuntime;
use orbit_engine::RuntimeHost;
use orbit_tools::ToolContext;
use orbit_types::task::TaskComplexity;
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

#[test]
fn ci_failure_branch_routing_retains_evidence_for_configurable_task_prefixes() {
    if !isolated("ci_failure_branch_routing_retains_evidence_for_configurable_task_prefixes") {
        return;
    }
    use orbit_core::application::task::TaskAddParams;
    use orbit_core::bootstrap::task_migration::seed_task_id_start;

    for prefix in ["DE", "ORBA", "ORBX", "ABCDE"] {
        let root = TempDir::new().unwrap();
        let global = root.path().join("home/.orbit");
        let workspace = root.path().join("repo/.orbit");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        seed_task_id_start(&global, Some(prefix), 1).unwrap();
        let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
        let owner = runtime
            .add_task(TaskAddParams {
                title: "Task branch owner".into(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(orbit_types::task::task_id_prefix(&owner.id), Some(prefix));

        let mut pr = failure("error: task branch regression", 0, &"3".repeat(40));
        pr["event"] = json!("pull_request");
        pr["head_branch"] = json!(format!("orbit/{}-ddb04571", owner.id));
        pr["ref_kind"] = json!("pull_request");

        let first = file(&runtime, vec![pr.clone()]);
        assert_eq!(first["filed_count"], 0, "{prefix}: {first}");
        assert_eq!(first["pilot_candidate_count"], 0);
        assert_eq!(first["excluded_branch_failures"], json!([]));
        assert_eq!(first["attributed"].as_array().unwrap().len(), 1);
        assert_eq!(first["attributed"][0]["task_id"], owner.id);
        let path = first["attributed"][0]["artifact"].as_str().unwrap();
        let artifact = runtime.get_task_artifact(&owner.id, path).unwrap().unwrap();
        let retained: Value = serde_json::from_slice(&artifact.content).unwrap();
        assert_eq!(retained["failure"], pr);
        assert_eq!(runtime.get_task(&owner.id).unwrap().status, owner.status);

        // Replaying in the collector's branch partition retains the same receipt.
        let repeated = runtime.run_deterministic("file_ci_failure_tasks", &json!({}), &json!({
            "ci_evidence": {
                "schema_version": 2, "collected": true,
                "heads": [{"kind": "integration", "branch": "agent-main", "current_head_sha": "1".repeat(40)}],
                "current_failures": [], "branch_failures": [pr],
            }
        }), ToolContext::default()).unwrap();
        assert_eq!(repeated["attributed"], first["attributed"]);
        assert_eq!(repeated["excluded_branch_failures"], json!([]));
        assert_eq!(runtime.get_task_artifacts(&owner.id).unwrap().len(), 1);
    }
}

/// The sweep routes each repair to a host that can reproduce it [ORB-14005]:
/// a failing job's runner labels tag it `os:macos` or `os:linux`, a workflow's
/// literal `runs-on` stands in when the snapshot carries no labels, and a
/// Windows or unrecognised runner leaves it untagged. The evidence is recorded
/// on the filing and in the description.
#[test]
fn ci_failure_sweep_tags_repairs_with_the_failing_runner_os() {
    if !isolated("ci_failure_sweep_tags_repairs_with_the_failing_runner_os") {
        return;
    }
    let root = TempDir::new().unwrap();
    let global = root.path().join("home/.orbit");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let workflows = root.path().join("repo/.github/workflows");
    std::fs::create_dir_all(&workflows).unwrap();
    std::fs::write(
        workflows.join("ci-macos.yml"),
        "name: macOS CI\non: push\njobs:\n  sandbox:\n    name: Sandbox\n    runs-on: macos-14\n    steps:\n      - run: make test\n",
    )
    .unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();

    let checkout = "3".repeat(40);
    // name, the job's runner labels (null: none), the workflow whose literal
    // `runs-on` stands in, the expected `os:` tag, and the evidence source.
    let cases = json!([
        {"name": "macos", "labels": ["macos-latest"], "tag": "os:macos", "source": "job_labels"},
        {"name": "ubuntu", "labels": ["ubuntu-24.04"], "tag": "os:linux", "source": "job_labels"},
        {"name": "windows", "labels": ["windows-latest"], "source": "job_labels"},
        {"name": "self-hosted", "labels": ["self-hosted", "gpu"], "source": "job_labels"},
        {"name": "runs-on", "workflow": "macOS CI", "tag": "os:macos", "source": "workflow_runs_on"},
        {"name": "unknown", "source": "unknown"},
    ]);
    let cases = cases.as_array().unwrap();
    let runs = cases
        .iter()
        .enumerate()
        .map(|(index, case)| {
            let mut run = failure(
                &format!(
                    "error: {} runner regression",
                    case["name"].as_str().unwrap()
                ),
                index,
                &checkout,
            );
            if !case["labels"].is_null() {
                run["failed_jobs"][0]["runner_labels"] = case["labels"].clone();
            }
            if !case["workflow"].is_null() {
                run["workflow"] = case["workflow"].clone();
                run["failed_jobs"][0]["name"] = json!("Sandbox");
            }
            run
        })
        .collect::<Vec<_>>();
    let output = runtime
        .run_deterministic(
            "file_ci_failure_tasks",
            &json!({}),
            &json!({"max_tasks": 10, "ci_evidence": {
                "schema_version": 2, "collected": true, "outcome_hint": "current_failures",
                "capability": {"available": true, "authenticated": true},
                "heads": [{"kind": "integration", "branch": "agent-main", "current_head_sha": "1".repeat(40)}],
                "latest_runs": runs.clone(), "current_failures": runs, "stale_or_superseded": [],
                "in_flight": [], "retryable_errors": [], "collected_at": "2026-10-04T08:00:00Z"
            }}),
            ToolContext::default(),
        )
        .expect("file CI failures");
    let filed = output["filed"].as_array().unwrap();
    assert_eq!(filed.len(), cases.len(), "{output}");

    for (index, case) in cases.iter().enumerate() {
        let name = &case["name"];
        let entry = filed
            .iter()
            .find(|entry| entry["run_ids"] == json!([10 + index]))
            .unwrap_or_else(|| panic!("{name} was filed: {output}"));
        let task = runtime
            .get_task(entry["task_id"].as_str().unwrap())
            .unwrap();
        let os_tags = task
            .tags
            .iter()
            .filter(|tag| tag.starts_with("os:"))
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(
            os_tags,
            case["tag"]
                .as_str()
                .map(|tag| vec![tag.to_string()])
                .unwrap_or_default(),
            "{name}: {:?}",
            task.tags
        );
        assert_eq!(
            entry["runner_os"][0]["source"], case["source"],
            "{name}: {entry}"
        );
        assert!(
            task.description.contains("- Runner OS: "),
            "{name}: the description records the runner evidence"
        );
    }
}

#[test]
fn ci_failure_sweep_uses_medium_pool_and_preserves_failure_key_tag() {
    if !isolated("ci_failure_sweep_uses_medium_pool_and_preserves_failure_key_tag") {
        return;
    }
    let root = TempDir::new().unwrap();
    let global = root.path().join("home/.orbit");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(
        workspace.join("config.toml"),
        r#"[workflow]
default_crew = "system"
medium_complexity_crews = ["fixture"]

[crews.fixture]
provider = "codex"
model = "fixture-model"
backend = "cli"

[crews.system]
provider = "codex"
model = "system-model"
backend = "cli"
"#,
    )
    .unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
    let output = file(
        &runtime,
        vec![failure(
            "error: pool routing regression",
            0,
            &"3".repeat(40),
        )],
    );
    let entry = &output["filed"][0];
    let task = runtime
        .get_task(entry["task_id"].as_str().unwrap())
        .unwrap();

    assert_eq!(task.complexity, Some(TaskComplexity::Medium));
    assert_eq!(task.crew.as_deref(), Some("fixture"));
    assert!(task.tags.contains(&format!(
        "ci-failure:{}",
        entry["failure_key"].as_str().unwrap()
    )));
}

/// Commit `count` empty commits on `repo` and return their ids, oldest first.
fn commit_chain(repo: &Path, count: usize) -> Vec<String> {
    let git = |args: &[&str]| {
        let mut command = std::process::Command::new("git");
        orbit_common::test_env::clear_inherited_authority(|key| {
            command.env_remove(key);
        });
        let output = command
            .args([
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.com",
            ])
            .args(args)
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    };
    git(&["init", "-q"]);
    (0..count)
        .map(|index| {
            git(&[
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                &format!("commit {index}"),
            ]);
            git(&["rev-parse", "HEAD"])
        })
        .collect()
}

/// Record a successful delivery run that committed and merged `task_id` at
/// `landed`, the way the host pipeline checkpoints it.
fn record_landing(runtime: &OrbitRuntime, task_id: &str, base: &str, landed: &str) {
    use orbit_types::workflow::{JobRunState, PipelineState};

    // A delivery job whose commit and merge steps are the shipped host actions.
    let resources = runtime.paths().global_dir.join("resources");
    std::fs::create_dir_all(resources.join("activities")).unwrap();
    std::fs::create_dir_all(resources.join("jobs")).unwrap();
    for activity in ["git_commit", "pr_complete"] {
        std::fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(format!("assets/activities/{activity}.yaml")),
            resources.join(format!("activities/{activity}.yaml")),
        )
        .unwrap();
    }
    std::fs::write(
        resources.join("jobs/fixture_delivery.yaml"),
        "schemaVersion: 2\nkind: Job\nmetadata:\n  name: fixture_delivery\nspec:\n  state: enabled\n  task_delivery: {}\n  steps:\n    - id: commit\n      target: activity:git_commit\n    - id: complete_pr\n      target: activity:pr_complete\n",
    )
    .unwrap();
    let mut run = runtime
        .insert_job_run(
            "fixture_delivery",
            1,
            chrono::Utc::now(),
            Some(json!({"task_ids": [task_id]})),
            None,
        )
        .unwrap();
    run.state = JobRunState::Success;
    runtime
        .sqlite_store()
        .unwrap()
        .upsert_job_run_for_workspace(&runtime.workspace_id().unwrap(), &run, None)
        .unwrap();
    let mut state = PipelineState::new(run.run_id.clone(), run.job_id.clone(), json!({}));
    for (index, step_id, output) in [
        (
            0,
            "commit",
            json!({
                "phase": "commit", "decision": "performed", "committed": true,
                "commit_sha": landed, "base_sha": base, "job_run_id": run.run_id, "task_id": task_id,
            }),
        ),
        (
            1,
            "complete_pr",
            json!({
                "phase": "complete",
                "merge": {"merged": true, "pr_number": "3507", "landed_commit": landed},
            }),
        ),
    ] {
        state.record_step(index, JobRunState::Success, Some(output.clone()), None);
        state.record_pipeline_output(step_id, output);
    }
    runtime.write_run_state(&run.run_id, &state).unwrap();
}

/// A red run that completes after its repair landed is not filed again
/// [ORB-14422]: the done repair with the same normalized signature landed at a
/// descendant of the tested commit, so the failure is held until a newer run
/// reproduces it. A failure tested on a commit that already contains the
/// landing is still filed, and collection's own holds pass through.
#[test]
fn ci_failure_predating_a_landed_repair_is_held_not_refiled() {
    if !isolated("ci_failure_predating_a_landed_repair_is_held_not_refiled") {
        return;
    }
    use orbit_engine::TaskAutomationUpdate;
    use orbit_types::task::TaskStatus;

    let root = TempDir::new().unwrap();
    let global = root.path().join("home/.orbit");
    let repo = root.path().join("repo");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(repo.join(".orbit")).unwrap();
    // first red run, a later red run on an older commit, the repair, and a
    // commit after the repair.
    let commits = commit_chain(&repo, 4);
    let (first, late, landed, after) = (&commits[0], &commits[1], &commits[2], &commits[3]);
    let runtime = OrbitRuntime::from_roots(&global, &repo.join(".orbit")).unwrap();
    let log = "error: landing stopped on its base is not repaired under the same task";

    let original = file(&runtime, vec![failure(log, 0, first)]);
    assert_eq!(original["filed_count"], 1, "{original}");
    let repair = original["filed"][0]["task_id"]
        .as_str()
        .unwrap()
        .to_string();
    runtime
        .apply_task_automation_update(
            &repair,
            TaskAutomationUpdate {
                status: Some(TaskStatus::Done),
                ..TaskAutomationUpdate::default()
            },
        )
        .unwrap();
    assert_eq!(runtime.get_task(&repair).unwrap().status, TaskStatus::Done);
    record_landing(&runtime, &repair, late, landed);

    // Another job with the same diagnostic, tested before the landing.
    let coverage = |index: usize, checkout: &str| {
        let mut run = failure(log, index, checkout);
        run["failed_jobs"][0]["name"] = json!("Coverage");
        run
    };
    let held = file(&runtime, vec![coverage(1, late)]);
    assert_eq!(held["filed_count"], 0, "{held}");
    assert_eq!(held["skipped_existing"], json!([]), "{held}");
    let pending = &held["pending_supersession"][0];
    assert_eq!(
        pending["reason"], "repaired_by_descendant_landing",
        "{held}"
    );
    assert_eq!(pending["task_id"], repair.as_str());
    assert_eq!(pending["landed_commit"], landed.as_str());
    assert_eq!(pending["tested_commit"], late.as_str());
    assert_eq!(pending["run_ids"], json!([11]));
    assert_eq!(held["audit"]["pending_supersession_run_ids"], json!([11]));

    // The landing is an ancestor of this checkout: the failure reproduces on
    // repaired code, so it is filed as before.
    let reproduced = file(&runtime, vec![coverage(2, after)]);
    assert_eq!(reproduced["filed_count"], 1, "{reproduced}");
    assert_eq!(reproduced["pending_supersession"], json!([]));

    // Collection's in-flight holds are reported by the filing step as well.
    let in_flight_hold = json!({
        "run_id": 37561651327u64, "workflow": "CI", "head_branch": "agent-main",
        "reason": "newer_descendant_run_in_flight",
        "pending_on": {"run_id": 37561800000u64, "status": "in_progress"},
    });
    let output = runtime.run_deterministic("file_ci_failure_tasks", &json!({}), &json!({
        "ci_evidence": {
            "schema_version": 2, "collected": true, "outcome_hint": "no_current_failure",
            "heads": [{"kind": "integration", "branch": "agent-main", "current_head_sha": "1".repeat(40)}],
            "current_failures": [], "pending_supersession": [in_flight_hold.clone()],
        }
    }), ToolContext::default()).unwrap();
    assert_eq!(output["outcome"], "no_current_failure");
    assert_eq!(output["pending_supersession"], json!([in_flight_hold]));
    assert_eq!(
        output["audit"]["pending_supersession_run_ids"],
        json!([37561651327u64])
    );
}

/// Collection releases a red run from `pending_supersession` despite a newer
/// push run still queued at a descendant commit when the previous completed
/// run failed the same way, or when the hold outlived its window [ORB-14610:
/// agent-main stayed red for 50 minutes because every sweep held the newest
/// red run behind the next queued push]. Filing files exactly one task for it
/// and the description names the run it did not wait for.
#[test]
fn ci_failure_released_from_pending_supersession_is_filed_naming_the_pending_run() {
    if !isolated("ci_failure_released_from_pending_supersession_is_filed_naming_the_pending_run") {
        return;
    }
    let log = "build\tRun CI\t2026-10-07T15:10:00.0000000Z ##[group]Run cargo check --workspace\n\
               build\tRun CI\t2026-10-07T15:10:00.0000000Z error[E0425]: cannot find value `probe_timeout` in this scope\n\
               build\tRun CI\t2026-10-07T15:10:00.0000000Z   --> crates/orbit-cmd/src/update/converge.rs:189:9\n\
               build\tRun CI\t2026-10-07T15:10:00.0000000Z ##[error]Process completed with exit code 101.\n";
    let queued = json!({
        "run_id": 37645190583u64, "status": "queued", "event": "push",
        "url": "https://github.com/acme/orbit/actions/runs/37645190583",
        "reported_head_sha": "9".repeat(40),
    });
    let previous = "5".repeat(40);
    let mut reproduced = failure(log, 1, &"6".repeat(40));
    reproduced["reproduced_on"] = json!({
        "run_id": 10, "url": "https://github.com/acme/orbit/actions/runs/10",
        "event_reported_head_sha": previous, "conclusion": "failure",
        "shared_failed_steps": [{"job": "build", "step": "Run CI"}],
        "pending_on": queued,
    });
    let mut held = failure(log, 2, &"7".repeat(40));
    held["held_past_window"] = json!({
        "pending_on": queued, "pending_since": "2026-10-07T15:10:00+00:00", "window_minutes": 30,
    });

    // What each release must name besides the pending run.
    for (released, expected) in [
        (
            reproduced,
            vec![
                "https://github.com/acme/orbit/actions/runs/10",
                previous.as_str(),
            ],
        ),
        (held, vec!["2026-10-07T15:10:00+00:00"]),
    ] {
        let root = TempDir::new().unwrap();
        let global = root.path().join("home/.orbit");
        let workspace = root.path().join("repo/.orbit");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();

        let output = file(&runtime, vec![released]);

        assert_eq!(output["filed_count"], 1, "{output}");
        assert_eq!(output["pending_supersession"], json!([]), "{output}");
        let task = runtime
            .get_task(output["filed"][0]["task_id"].as_str().unwrap())
            .unwrap();
        let pending = "9".repeat(40);
        for named in expected.into_iter().chain([
            "https://github.com/acme/orbit/actions/runs/37645190583",
            pending.as_str(),
        ]) {
            assert!(
                task.description.contains(named),
                "the filed description names {named}: {}",
                task.description
            );
        }
        assert!(
            task.description.contains("- Compiler cause identity: `"),
            "{}",
            task.description
        );
    }
}
