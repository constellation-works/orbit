//! CLI wait deadlines leave active runs observable under their actual state.

use std::fs;

use serde_json::Value;

use super::{FAILED, Fixture, isolated_run_observation};

fn wait_expiry_reports_active_state_without_timing_out_the_run() {
    let fixture = Fixture::init();
    let jobs = fixture.home.join(".orbit/resources/jobs");
    fs::create_dir_all(&jobs).unwrap();
    // Keep both entry points active regardless of scheduler load. Cancel each
    // worker after observing it so the fixture leaves no detached children.
    for name in ["wait_fixture", "task_pilot_pipeline"] {
        fs::write(
            jobs.join(format!("{name}.yaml")),
            serde_json::json!({
                "schemaVersion": 2, "kind": "Job", "metadata": {"name": name},
                "spec": {"state": "enabled", "kind": "workflow", "steps": [{
                    "id": "nap", "default_input": {"seconds": 300},
                    "spec": {"type": "deterministic", "action": "sleep", "config": {}}
                }]}
            })
            .to_string(),
        )
        .unwrap();
    }
    for args in [
        vec!["run", "job", "wait_fixture"],
        vec!["run", "task-pilot"],
    ] {
        for as_json in [true, false] {
            let mut command = fixture.orbit();
            command
                .env("ORBIT_TEST_PIPELINE_WAIT_VIRTUAL", "1")
                .args(&args)
                .args(["--wait", "--timeout-seconds", "1"]);
            if as_json {
                command.arg("--json");
            }
            let output = command
                .timeout(std::time::Duration::from_secs(60))
                .assert()
                .code(1)
                .get_output()
                .clone();
            let run_id = if as_json {
                let result: Value = serde_json::from_slice(&output.stdout).unwrap();
                assert_eq!(result["wait_timeout"], true, "{result}");
                assert!(
                    matches!(result["state"].as_str(), Some("pending" | "running")),
                    "{result}"
                );
                if args[1] == "job" {
                    assert!(result["finished_at"].is_null(), "{result}");
                    assert!(result["error"].is_null(), "{result}");
                } else {
                    assert!(result["error_code"].is_null(), "{result}");
                    assert!(result["error_message"].is_null(), "{result}");
                }
                result["run_id"].as_str().unwrap().to_string()
            } else {
                let text = String::from_utf8(output.stdout).unwrap();
                assert!(text.contains("wait_timeout=true"), "{text}");
                if args[1] == "job" {
                    assert!(
                        text.contains("State: pending") || text.contains("State: running"),
                        "{text}"
                    );
                    text.lines()
                        .find_map(|line| line.strip_prefix("Run ID: "))
                        .unwrap()
                        .to_string()
                } else {
                    assert!(
                        text.contains("state=pending;") || text.contains("state=running;"),
                        "{text}"
                    );
                    text.split(';')
                        .find_map(|field| field.strip_prefix("run_id="))
                        .unwrap()
                        .to_string()
                }
            };
            assert!(matches!(
                fixture.run_state(&run_id).as_str(),
                "pending" | "running"
            ));
            let shown = fixture.json(&["run", "show", &run_id, "--no-reconcile", "--json"]);
            assert!(shown["run"]["finished_at"].is_null(), "{shown}");
            fixture.json(&["run", "cancel", &run_id, "--confirm", "--json"]);
            assert_eq!(fixture.run_state(&run_id), "cancelled");
        }
    }
}

fn invalid_wait_deadline_does_not_submit_work() {
    let fixture = Fixture::init();
    // Runtime open may reconcile the fixture's pre-seeded stale runs. Finish
    // that normal repair before comparing invalid submissions for mutations.
    fixture.json(&["run", "history", "--json"]);
    let before = fixture.snapshot();
    for args in [
        vec!["run", "job", "task_pilot_pipeline"],
        vec!["run", "task-pilot"],
        vec!["job", "resume", FAILED],
    ] {
        let mut command = fixture.orbit();
        let output = command
            .args(&args)
            .args(["--wait", "--timeout-seconds", "21601", "--json"])
            .assert()
            .failure()
            .get_output()
            .clone();
        let error: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert!(error.to_string().contains("must be <= 21600"), "{error}");
        fixture
            .orbit()
            .args(&args)
            .args(["--timeout-seconds", "1"])
            .assert()
            .code(2);
    }
    assert_eq!(
        fixture.snapshot(),
        before,
        "invalid wait options must not submit any runs"
    );
}

#[cfg(unix)]
fn explicit_task_pilot_refuses_unsupported_status_before_submission() {
    use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
    use orbit_core::{OrbitRuntime, TaskStatus};
    use std::os::unix::fs::PermissionsExt;

    let fixture = Fixture::terminal();
    fs::write(fixture.work.join("README.md"), "fixture\n").unwrap();
    for args in [
        vec!["config", "user.name", "Orbit Test"],
        vec!["config", "user.email", "orbit-test@example.com"],
        vec!["config", "commit.gpgsign", "false"],
        vec!["add", "README.md"],
        vec!["commit", "-m", "seed"],
    ] {
        crate::git_repo::run(&fixture.work, &args);
    }
    let runtime =
        OrbitRuntime::from_roots(&fixture.home.join(".orbit"), &fixture.work.join(".orbit"))
            .unwrap();
    let calls = fixture.home.join("provider-calls");
    fs::write(&calls, "").unwrap();
    let response = fixture.home.join("pilot-response.json");
    let provider = fixture.home.join("empty-bin/codex");
    fs::write(
        &provider,
        format!(
            "#!/bin/sh\n/bin/cat > /dev/null\nprintf 'call\\n' >> '{}'\n/bin/cat '{}'\n",
            calls.display(),
            response.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&provider, fs::Permissions::from_mode(0o755)).unwrap();
    let assets = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../orbit-core/assets");
    let mut executor: orbit_types::resource::ExecutorResource =
        serde_yaml::from_str(&fs::read_to_string(assets.join("executors/codex.yaml")).unwrap())
            .unwrap();
    executor.spec.command = Some(provider.display().to_string());
    executor.spec.sandbox = Some(orbit_types::workflow::ExecutorSandboxKind::Off);
    runtime
        .upsert_executor_def(&orbit_types::workflow::ExecutorDef::from_resource_spec(
            executor.metadata.name,
            executor.spec,
            None,
            None,
        ))
        .unwrap();
    let add = |title: &str| {
        runtime
            .add_task(TaskAddParams {
                title: title.into(),
                description: "Explicit pilot eligibility fixture".into(),
                acceptance_criteria: vec!["Assess only eligible tasks".into()],
                plan: "Inspect README.md".into(),
                tags: vec!["delivery:task_local_pipeline".into()],
                status: Some(TaskStatus::Proposed),
                ..Default::default()
            })
            .unwrap()
    };
    let allowed = add("eligible sibling");
    let mut unsupported = Vec::new();
    for status in [
        TaskStatus::Blocked,
        TaskStatus::InProgress,
        TaskStatus::Review,
        TaskStatus::Done,
        TaskStatus::Archived,
        TaskStatus::Rejected,
        TaskStatus::Someday,
    ] {
        let task = add(&format!("unsupported {status}"));
        runtime
            .force_update_task_with_identity(
                &task.id,
                TaskUpdateParams {
                    status: Some(status),
                    ..Default::default()
                },
                None,
                None,
            )
            .unwrap();
        unsupported.push((task.id, status));
    }
    // Each unsupported state, then one mixed selection naming all of them.
    let mut selections = unsupported
        .iter()
        .map(|(id, status)| (vec![id.as_str()], vec![(id.as_str(), *status)]))
        .collect::<Vec<_>>();
    selections.push((
        std::iter::once(allowed.id.as_str())
            .chain(unsupported.iter().map(|(id, _)| id.as_str()))
            .collect(),
        unsupported
            .iter()
            .map(|(id, status)| (id.as_str(), *status))
            .collect(),
    ));
    let open = || {
        OrbitRuntime::from_roots(&fixture.home.join(".orbit"), &fixture.work.join(".orbit"))
            .unwrap()
    };
    let snapshot = || {
        let runtime = open();
        std::iter::once(allowed.id.as_str())
            .chain(unsupported.iter().map(|(id, _)| id.as_str()))
            .map(|id| {
                serde_json::json!({"task": runtime.get_task(id).unwrap(),
                "history": runtime.get_task_history(id).unwrap()})
            })
            .collect::<Vec<_>>()
    };
    let before = snapshot();
    let runs_before = fixture.snapshot();
    let reservations = runtime
        .paths()
        .state_dir
        .join("task-pilot-reservations.json");
    let reservation_before = fs::read(&reservations).ok();
    for (ids, refused) in selections {
        let mut command = fixture.orbit();
        command.env("PATH", std::env::var_os("PATH").unwrap());
        let output = command
            .args(["run", "task-pilot"])
            .args(ids)
            .args(["--wait", "--timeout-seconds", "30", "--json"])
            .timeout(std::time::Duration::from_secs(60))
            .output()
            .unwrap();
        assert!(
            !output.status.success(),
            "invalid explicit status must refuse: {output:?}"
        );
        let error: Value = serde_json::from_slice(&output.stderr)
            .unwrap_or_else(|_| panic!("refusal must precede submission: {output:?}"));
        assert_eq!(error["code"], "invalid_input", "{error}");
        let message = error.to_string();
        for (id, status) in refused {
            assert!(
                message.contains(id) && message.contains(&status.to_string()),
                "{message}"
            );
        }
        assert!(
            message.contains("proposed") && message.contains("backlog"),
            "{message}"
        );
        assert_eq!(
            snapshot(),
            before,
            "refusal must leave task records/history unchanged"
        );
        assert_eq!(
            fixture.snapshot(),
            runs_before,
            "refusal must not submit work"
        );
        assert_eq!(fs::read(&reservations).ok(), reservation_before);
        assert_eq!(
            fs::read_to_string(&calls).unwrap(),
            "",
            "zero provider invocations"
        );
    }
    for status in [TaskStatus::Proposed, TaskStatus::Backlog] {
        let task = add(&format!("eligible {status}"));
        if status == TaskStatus::Backlog {
            runtime
                .update_task_as_human(
                    &task.id,
                    TaskUpdateParams {
                        status: Some(status),
                        ..Default::default()
                    },
                    "fixture".into(),
                )
                .unwrap();
        }
        let observed_revision = crate::git_repo::run(&fixture.work, &["rev-parse", "HEAD"]);
        let observed_tree = crate::git_repo::run(&fixture.work, &["rev-parse", "HEAD^{tree}"]);
        fs::write(
            &response,
            serde_json::json!({
                "schemaVersion": 1, "status": "success", "error": null,
                "result": {"partition_index": 0, "observed_revision": observed_revision,
                    "observed_tree": observed_tree, "task_ids": [task.id], "tasks": [{
                    "task_id": task.id, "context_files_after": ["file:README.md"],
                    "disposition": "selectors", "recommended_crew": "sol",
                    "recommended_complexity": "low", "confidence": "high",
                    "assessment_rationale": "README.md identifies the fixture scope.",
                    "validation_approach": "Inspect persisted selectors.",
                    "evidence_gaps": [], "reassessment_triggers": [], "blocked_by": [],
                    "adr_conflicts": [], "utility_warnings": [], "surface_warnings": [],
                    "duplicate_of": null, "already_landed": null
                }]}
            })
            .to_string(),
        )
        .unwrap();
        let output = fixture
            .orbit()
            .env("PATH", std::env::var_os("PATH").unwrap())
            .args([
                "run",
                "task-pilot",
                &task.id,
                "--wait",
                "--timeout-seconds",
                "30",
                "--json",
            ])
            .timeout(std::time::Duration::from_secs(60))
            .output()
            .unwrap();
        assert!(output.status.success(), "{status}: {output:?}");
        let submitted: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(submitted["state"], "success", "{submitted}");
        let shown = fixture.json(&[
            "run",
            "show",
            submitted["run_id"].as_str().unwrap(),
            "--json",
        ]);
        assert_eq!(
            shown["pipeline_state"]["pipeline"]["apply"]["applied_count"], 1,
            "{shown}"
        );
        let after = open().get_task(&task.id).unwrap();
        assert_eq!(after.status, status);
        assert_eq!(after.context_files, ["file:README.md"]);
    }
    assert_eq!(fs::read_to_string(calls).unwrap().lines().count(), 2);
}

#[cfg(unix)]
fn explicit_task_pilot_partitions_mixed_crews_before_submission() {
    use orbit_core::application::task::TaskAddParams;
    use orbit_core::{OrbitRuntime, TaskComplexity, TaskStatus, TaskType};
    use orbit_types::resource::ExecutorResource;
    use orbit_types::workflow::{ExecutorDef, ExecutorSandboxKind, JobRunState};
    use std::os::unix::fs::PermissionsExt;

    let fixture = Fixture::terminal();
    fs::write(fixture.work.join("README.md"), "fixture\n").unwrap();
    for args in [
        vec!["config", "user.name", "Orbit Test"],
        vec!["config", "user.email", "orbit-test@example.com"],
        vec!["config", "commit.gpgsign", "false"],
        vec!["add", "README.md"],
        vec!["commit", "-m", "seed"],
    ] {
        crate::git_repo::run(&fixture.work, &args);
    }
    let config_path = fixture.home.join(".orbit/config.toml");
    let mut config: toml_edit::DocumentMut =
        fs::read_to_string(&config_path).unwrap().parse().unwrap();
    config["crews"]["haiku"]["enabled"] = toml_edit::value(true);
    config["crews"]["haiku"]["provider"] = toml_edit::value("codex");
    config["crews"]["haiku"]["model"] = toml_edit::value("fixture-model");
    config["crews"]["haiku"]["backend"] = toml_edit::value("cli");
    fs::write(&config_path, config.to_string()).unwrap();

    let runtime =
        OrbitRuntime::from_roots(&fixture.home.join(".orbit"), &fixture.work.join(".orbit"))
            .unwrap();
    let provider = fixture.home.join("empty-bin/codex");
    fs::write(
        &provider,
        r##"#!/usr/bin/env python3
import json, re, subprocess, sys
prompt = sys.stdin.read()
ids = []
for blob in re.findall(r'"task_ids"\s*:\s*(\[[^\]]*\])', prompt):
    try:
        parsed = json.loads(blob)
    except json.JSONDecodeError:
        continue
    ids = [item for item in parsed if isinstance(item, str) and re.fullmatch(r"ORB-\d+", item)]
    if ids:
        break
if not ids:
    sys.stderr.write("provider saw no task ids\n")
    sys.exit(1)
head = subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip()
tree = subprocess.check_output(["git", "rev-parse", "HEAD^{tree}"], text=True).strip()
tasks = [{
    "task_id": task_id,
    "context_files_after": ["file:README.md"],
    "disposition": "selectors",
    "recommended_crew": "fixture",
    "recommended_complexity": "low",
    "confidence": "high",
    "assessment_rationale": "README.md identifies the fixture scope.",
    "validation_approach": "Inspect persisted selectors.",
    "evidence_gaps": [],
    "reassessment_triggers": [],
    "blocked_by": [],
    "adr_conflicts": [],
    "utility_warnings": [],
    "surface_warnings": [],
    "duplicate_of": None,
    "already_landed": None,
} for task_id in ids]
sys.stdout.write(json.dumps({
    "schemaVersion": 1,
    "status": "success",
    "error": None,
    "result": {
        "partition_index": 0,
        "observed_revision": head,
        "observed_tree": tree,
        "task_ids": ids,
        "tasks": tasks,
    },
}))
"##,
    )
    .unwrap();
    fs::set_permissions(&provider, fs::Permissions::from_mode(0o755)).unwrap();
    let assets = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../orbit-core/assets");
    let mut executor: ExecutorResource =
        serde_yaml::from_str(&fs::read_to_string(assets.join("executors/codex.yaml")).unwrap())
            .unwrap();
    executor.spec.command = Some(provider.display().to_string());
    executor.spec.sandbox = Some(ExecutorSandboxKind::Off);
    runtime
        .upsert_executor_def(&ExecutorDef::from_resource_spec(
            executor.metadata.name,
            executor.spec,
            None,
            None,
        ))
        .unwrap();
    let add = |title: &str, crew: &str| {
        runtime
            .add_task(TaskAddParams {
                title: title.into(),
                description: "Explicit mixed-crew pilot fixture".into(),
                acceptance_criteria: vec!["Assess only eligible tasks".into()],
                plan: "Inspect README.md".into(),
                context_files: vec!["file:README.md".into()],
                tags: vec!["delivery:task_local_pipeline".into()],
                complexity: TaskComplexity::Low,
                task_type: Some(TaskType::Feature),
                status: Some(TaskStatus::Proposed),
                crew: Some(crew.into()),
                ..Default::default()
            })
            .unwrap()
    };
    let haiku = add("haiku pilot task", "haiku");
    let sol = add("sol pilot task", "sol");

    let output = fixture
        .orbit()
        .env("PATH", std::env::var_os("PATH").unwrap())
        .args([
            "run",
            "task-pilot",
            &haiku.id,
            &sol.id,
            "--wait",
            "--timeout-seconds",
            "120",
            "--json",
        ])
        .timeout(std::time::Duration::from_secs(180))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "mixed-crew selection must be partitioned before run start: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let submitted: Value = serde_json::from_slice(&output.stdout).unwrap();
    let runs = submitted["runs"].as_array().expect("two crew partitions");
    assert_eq!(runs.len(), 2, "{submitted}");
    let mut task_ids = Vec::new();
    for entry in runs {
        assert_eq!(entry["state"], "success", "{entry}");
        let run = runtime
            .show_job_run(entry["run_id"].as_str().unwrap())
            .unwrap();
        assert_eq!(run.state, JobRunState::Success, "{run:?}");
        let state = runtime
            .read_run_state(&run.run_id)
            .unwrap()
            .expect("completed pilot run state");
        assert_eq!(
            state.pipeline["prepare"]["task_ids"],
            serde_json::json!(run.input.as_ref().unwrap()["task_ids"]),
            "each crew run must prepare its selected task: {:#}",
            state.pipeline["prepare"]
        );
        let ids = run
            .input
            .as_ref()
            .and_then(|input| input["task_ids"].as_array())
            .expect("each run carries an explicit crew-homogeneous selection");
        assert_eq!(ids.len(), 1, "mixed-crew run input: {run:?}");
        task_ids.push(ids[0].as_str().unwrap().to_string());
    }
    task_ids.sort();
    let mut expected = vec![haiku.id, sol.id];
    expected.sort();
    assert_eq!(task_ids, expected);
}

/// A provider-capacity pilot failure ends `held`: an accepted async
/// submission still exits zero, while `--wait` exits nonzero with
/// `state=held` and the provider diagnostic in both text and JSON.
#[cfg(unix)]
fn task_pilot_wait_held_provider_capacity_exits_nonzero() {
    use orbit_core::application::task::TaskAddParams;
    use orbit_core::{OrbitRuntime, TaskStatus};
    use std::os::unix::fs::PermissionsExt;

    let fixture = Fixture::terminal();
    fs::write(fixture.work.join("README.md"), "fixture\n").unwrap();
    for args in [
        vec!["config", "user.name", "Orbit Test"],
        vec!["config", "user.email", "orbit-test@example.com"],
        vec!["config", "commit.gpgsign", "false"],
        vec!["add", "README.md"],
        vec!["commit", "-m", "seed"],
    ] {
        crate::git_repo::run(&fixture.work, &args);
    }
    let runtime =
        OrbitRuntime::from_roots(&fixture.home.join(".orbit"), &fixture.work.join(".orbit"))
            .unwrap();
    let calls = fixture.home.join("provider-calls-held");
    fs::write(&calls, "").unwrap();
    let provider = fixture.home.join("empty-bin/codex");
    fs::write(
        &provider,
        format!(
            "#!/bin/sh\n/bin/cat > /dev/null\nprintf 'call\\n' >> '{}'\nprintf '%s\\n' '{{\"type\":\"turn.failed\",\"error\":{{\"message\":\"Selected model is at capacity. Please try a different model.\"}}}}'\nexit 1\n",
            calls.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&provider, fs::Permissions::from_mode(0o755)).unwrap();
    let assets = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../orbit-core/assets");
    let mut executor: orbit_types::resource::ExecutorResource =
        serde_yaml::from_str(&fs::read_to_string(assets.join("executors/codex.yaml")).unwrap())
            .unwrap();
    executor.spec.command = Some(provider.display().to_string());
    executor.spec.sandbox = Some(orbit_types::workflow::ExecutorSandboxKind::Off);
    runtime
        .upsert_executor_def(&orbit_types::workflow::ExecutorDef::from_resource_spec(
            executor.metadata.name,
            executor.spec,
            None,
            None,
        ))
        .unwrap();
    let add = |title: &str| {
        runtime
            .add_task(TaskAddParams {
                title: title.into(),
                description: "Held pilot fixture".into(),
                acceptance_criteria: vec!["Assess only eligible tasks".into()],
                plan: "Inspect README.md".into(),
                tags: vec!["delivery:task_local_pipeline".into()],
                status: Some(TaskStatus::Proposed),
                ..Default::default()
            })
            .unwrap()
            .id
    };

    // Accepted asynchronous submission exits zero even though the run
    // later holds; poll the durable record until it reaches held.
    let async_task = add("held async pilot");
    let output = fixture
        .orbit()
        .env("PATH", std::env::var_os("PATH").unwrap())
        .args(["run", "task-pilot", &async_task, "--json"])
        .timeout(std::time::Duration::from_secs(60))
        .output()
        .unwrap();
    assert!(output.status.success(), "async submission: {output:?}");
    let submitted: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        matches!(submitted["state"].as_str(), Some("submitted" | "queued")),
        "{submitted}"
    );
    let async_run = submitted["run_id"].as_str().unwrap().to_string();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    while fixture.run_state(&async_run) != "held" {
        assert!(
            std::time::Instant::now() < deadline,
            "async run {async_run} did not reach held; state={}",
            fixture.run_state(&async_run)
        );
        std::thread::sleep(std::time::Duration::from_millis(500));
    }

    for as_json in [true, false] {
        let task = add(if as_json {
            "held json pilot"
        } else {
            "held text pilot"
        });
        let mut command = fixture.orbit();
        command
            .env("PATH", std::env::var_os("PATH").unwrap())
            .args([
                "run",
                "task-pilot",
                &task,
                "--wait",
                "--timeout-seconds",
                "120",
            ])
            .timeout(std::time::Duration::from_secs(180));
        if as_json {
            command.arg("--json");
        }
        let output = command.output().unwrap();
        assert_eq!(
            output.status.code(),
            Some(1),
            "held --wait must exit nonzero: {output:?}"
        );
        if as_json {
            let result: Value = serde_json::from_slice(&output.stdout)
                .unwrap_or_else(|_| panic!("held JSON: {output:?}"));
            assert_eq!(result["state"], "held", "{result}");
            assert_eq!(result["wait_timeout"], false, "{result}");
            let diagnostic = format!(
                "{} {}",
                result["error_code"].as_str().unwrap_or(""),
                result["error_message"].as_str().unwrap_or("")
            );
            assert!(
                diagnostic.contains("capacity") || diagnostic.contains("at capacity"),
                "held JSON must retain the provider diagnostic: {result}"
            );
            assert_eq!(
                fixture.run_state(result["run_id"].as_str().unwrap()),
                "held"
            );
        } else {
            let text = String::from_utf8_lossy(&output.stdout).into_owned();
            assert!(text.contains("state=held"), "{text}");
            assert!(
                text.contains("capacity"),
                "held text must retain the provider diagnostic: {text}"
            );
        }
    }
    assert!(
        fs::read_to_string(&calls).unwrap().lines().count() >= 3,
        "provider must have been invoked for each pilot run"
    );
}

#[test]
fn wait_contracts() {
    if !isolated_run_observation("run_observation::wait::wait_contracts") {
        return;
    }
    for (name, check) in [
        #[cfg(unix)]
        (
            "task_pilot_wait_held_provider_capacity_exits_nonzero",
            task_pilot_wait_held_provider_capacity_exits_nonzero as fn(),
        ),
        #[cfg(unix)]
        (
            "explicit_task_pilot_refuses_unsupported_status_before_submission",
            explicit_task_pilot_refuses_unsupported_status_before_submission as fn(),
        ),
        (
            "wait_expiry_reports_active_state_without_timing_out_the_run",
            wait_expiry_reports_active_state_without_timing_out_the_run as fn(),
        ),
        (
            "invalid_wait_deadline_does_not_submit_work",
            invalid_wait_deadline_does_not_submit_work as fn(),
        ),
    ] {
        tracing::info!(case = name, "CLI contract row");
        check();
    }
}

#[test]
fn explicit_task_pilot_mixed_crew_contract() {
    if !isolated_run_observation("run_observation::wait::explicit_task_pilot_mixed_crew_contract") {
        return;
    }
    explicit_task_pilot_partitions_mixed_crews_before_submission();
}
