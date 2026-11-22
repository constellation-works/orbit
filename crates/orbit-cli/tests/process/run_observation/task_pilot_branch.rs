//! A commit passed as a pilot branch is refused before a run or task write.

use std::fs;
use std::time::Duration;

use orbit_common::process::identity::{ProcessLiveness, probe_process_liveness};
use orbit_common::test_env;
use orbit_core::application::task::TaskAddParams;
use serde_json::{Value, json};

use super::{Fixture, isolated_run_observation};
use crate::git_repo;

#[test]
fn task_pilot_branch_admission_preserves_source_identity() {
    if !isolated_run_observation(
        "run_observation::task_pilot_branch::task_pilot_branch_admission_preserves_source_identity",
    ) {
        return;
    }
    let fixture = Fixture::terminal();
    git_repo::SeededRepo::new(&fixture.work).build();
    let pinned = git_repo::run(&fixture.work, &["rev-parse", "HEAD"]);
    fs::write(fixture.work.join("README.md"), "new branch source\n").unwrap();
    git_repo::run(&fixture.work, &["commit", "-am", "advance branch"]);
    let tip = git_repo::run(&fixture.work, &["rev-parse", "HEAD"]);
    let origin = fixture.home.join("origin.git");
    git_repo::run(
        &fixture.work,
        &["clone", "--bare", ".", origin.to_str().unwrap()],
    );
    git_repo::run(
        &fixture.work,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    let runtime = fixture.registered_runtime();
    let task = runtime
        .add_task(TaskAddParams {
            title: "Pilot branch admission".into(),
            acceptance_criteria: vec!["Inspect the requested source".into()],
            ..Default::default()
        })
        .unwrap();
    let before_task = serde_json::to_value(runtime.get_task(&task.id).unwrap()).unwrap();
    let before_history = serde_json::to_value(runtime.get_task_history(&task.id).unwrap()).unwrap();
    let before_runs = fixture.snapshot();
    // Limit successful controls to the real prepare boundary, without providers
    // or apply writes. Preparation still resolves the requested source.
    fixture.write_job(
        "task_pilot_pipeline.yaml",
        &json!({"schemaVersion": 2, "kind": "Job", "metadata": {"name": "task_pilot_pipeline"},
        "spec": {"state": "enabled", "kind": "workflow", "steps": [{"id": "prepare",
            "spec": {"type": "deterministic", "action": "prepare_task_pilot", "config": {}}
        }]}})
        .to_string(),
    );
    let cli = |args: &[&str]| {
        let output = fixture
            .orbit()
            .env("PATH", std::env::var_os("PATH").unwrap())
            .args(args)
            .timeout(Duration::from_secs(120))
            .output()
            .unwrap();
        if let Ok(result) = serde_json::from_slice::<Value>(&output.stdout)
            && let Some(id) = result["run_id"].as_str()
        {
            let run = runtime.show_job_run(id).unwrap();
            test_env::wait_until("disposable pilot worker exit", || {
                run.pid.is_none_or(|pid| {
                    probe_process_liveness(pid, run.pid_start_time.as_deref())
                        == ProcessLiveness::Exited
                })
            });
        }
        output
    };
    for branch in [pinned.clone(), format!(" origin/{pinned} ")] {
        let output = cli(&[
            "run",
            "task-pilot",
            &task.id,
            "--base-branch",
            &branch,
            "--wait",
            "--timeout-seconds",
            "60",
            "--json",
        ]);
        assert!(!output.status.success(), "{output:?}");
        let diagnostic = String::from_utf8_lossy(&output.stderr);
        assert!(
            diagnostic.contains("--base-branch requires a branch name"),
            "{diagnostic}"
        );
        assert!(
            diagnostic.contains("--input source_revision=<FULL_COMMIT_ID>"),
            "{diagnostic}"
        );
        assert_eq!(
            fixture.snapshot(),
            before_runs,
            "a mistaken revision must create no pilot run"
        );
        assert_eq!(
            serde_json::to_value(runtime.get_task(&task.id).unwrap()).unwrap(),
            before_task
        );
        assert_eq!(
            serde_json::to_value(runtime.get_task_history(&task.id).unwrap()).unwrap(),
            before_history
        );
    }
    // Give the commit's exact hex spelling a real remote branch at a *different*
    // commit. Stale tracking and local branches must not decide the pin.
    git_repo::run(
        &fixture.work,
        &["push", "origin", &format!("HEAD:refs/heads/{pinned}")],
    );
    git_repo::run(
        &fixture.work,
        &[
            "update-ref",
            &format!("refs/remotes/origin/{pinned}"),
            &pinned,
        ],
    );
    git_repo::run(&fixture.work, &["update-ref", "refs/heads/main", &pinned]);
    for branch in [
        "main".to_string(),
        pinned.clone(),
        format!("origin/{pinned}"),
    ] {
        let output = cli(&[
            "run",
            "task-pilot",
            "--base-branch",
            &branch,
            "--max-tasks",
            "1",
            "--wait",
            "--timeout-seconds",
            "60",
            "--json",
        ]);
        assert!(output.status.success(), "{output:?}");
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        let state = runtime
            .read_run_state(result["run_id"].as_str().unwrap())
            .unwrap()
            .unwrap();
        let source = &state.pipeline["prepare"]["source"];
        let normalized = branch.strip_prefix("origin/").unwrap_or(&branch);
        assert_eq!(source["source_ref"], format!("origin/{normalized}"));
        assert_eq!(source["base_branch"], normalized);
        assert_eq!(
            source["source_revision"], tip,
            "a real branch must pin its fetched head"
        );
    }
    let output = cli(&[
        "run",
        "job",
        "task_pilot_pipeline",
        "--input",
        "base_branch=main",
        "--input",
        &format!("source_revision={pinned}"),
        "--input",
        "max_tasks=1",
        "--wait",
        "--timeout-seconds",
        "60",
        "--json",
    ]);
    assert!(output.status.success(), "{output:?}");
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    let state = runtime
        .read_run_state(result["run_id"].as_str().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(state.pipeline["prepare"]["source"]["base_branch"], "main");
    assert_eq!(state.pipeline["prepare"]["source"]["source_ref"], pinned);
    assert_eq!(
        state.pipeline["prepare"]["source"]["source_revision"],
        pinned
    );
    assert_eq!(git_repo::run(&fixture.work, &["rev-parse", "HEAD"]), pinned);
}

#[test]
fn generic_job_run_rejects_string_task_ids_for_task_pilot_before_submission() {
    if !isolated_run_observation(
        "run_observation::task_pilot_branch::generic_job_run_rejects_string_task_ids_for_task_pilot_before_submission",
    ) {
        return;
    }
    let fixture = Fixture::terminal();
    git_repo::SeededRepo::new(&fixture.work).build();
    let origin = fixture.home.join("origin.git");
    git_repo::run(
        &fixture.work,
        &["clone", "--bare", ".", origin.to_str().unwrap()],
    );
    git_repo::run(
        &fixture.work,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    let runtime = fixture.registered_runtime();
    let task = runtime
        .add_task(TaskAddParams {
            title: "Generic job-run task_ids rejection fixture".into(),
            acceptance_criteria: vec!["Reject string task_ids before run submission".into()],
            tags: vec!["delivery:task_local_pipeline".into()],
            ..Default::default()
        })
        .unwrap();

    let pilot_job = json!({
        "schemaVersion": 2,
        "kind": "Job",
        "metadata": {"name": "task_pilot_pipeline"},
        "spec": {
            "state": "enabled",
            "kind": "workflow",
            "steps": [{
                "id": "prepare",
                "spec": {"type": "deterministic", "action": "prepare_task_pilot", "config": {}}
            }]
        }
    });
    fixture.write_job("task_pilot_pipeline.yaml", &pilot_job.to_string());
    let direct_pilot_path = fixture.home.join("direct-pilot.yaml");
    fs::write(&direct_pilot_path, pilot_job.to_string()).unwrap();

    // Unrelated job without prepare_task_pilot must retain string `--input` interpretation.
    fixture.write_job(
        "string_input_probe.yaml",
        &json!({
            "schemaVersion": 2,
            "kind": "Job",
            "metadata": {"name": "string_input_probe"},
            "spec": {
                "state": "enabled",
                "kind": "workflow",
                "steps": [{
                    "id": "nap",
                    "default_input": {"seconds": 0},
                    "spec": {"type": "deterministic", "action": "sleep", "config": {}}
                }]
            }
        })
        .to_string(),
    );

    let cli = |args: &[&str]| {
        let output = fixture
            .orbit()
            .env("PATH", std::env::var_os("PATH").unwrap())
            .args(args)
            .timeout(Duration::from_secs(120))
            .output()
            .unwrap();
        if let Ok(result) = serde_json::from_slice::<Value>(&output.stdout)
            && let Some(id) = result["run_id"].as_str()
        {
            let run = runtime.show_job_run(id).unwrap();
            test_env::wait_until("disposable pilot worker exit", || {
                run.pid.is_none_or(|pid| {
                    probe_process_liveness(pid, run.pid_start_time.as_deref())
                        == ProcessLiveness::Exited
                })
            });
        }
        output
    };

    let before_runs = fixture.snapshot();
    let direct_pilot_arg = direct_pilot_path.to_str().unwrap().to_string();
    let malformed_bracket = format!("task_ids=[{}", task.id);
    let unquoted_bracket = format!("task_ids=[{}]", task.id);
    let json_string_array = format!("task_ids=[\"{}\"]", task.id);
    let bare_id = format!("task_ids={}", task.id);

    for target in ["task_pilot_pipeline", direct_pilot_arg.as_str()] {
        for subcommand in [["run", "job"], ["job", "run"]] {
            for input_arg in [
                unquoted_bracket.as_str(),
                json_string_array.as_str(),
                bare_id.as_str(),
                malformed_bracket.as_str(),
                "task_ids=",
                "task_ids=[]",
            ] {
                let output = cli(&[
                    subcommand[0],
                    subcommand[1],
                    target,
                    "--input",
                    input_arg,
                    "--json",
                ]);
                assert_eq!(
                    output.status.code(),
                    Some(1),
                    "{subcommand:?} {target} {input_arg} must fail before submit: {output:?}"
                );
                assert!(
                    output.stdout.is_empty(),
                    "failed JSON invocation must leave stdout empty: {output:?}"
                );
                let error: Value = serde_json::from_slice(&output.stderr)
                    .unwrap_or_else(|err| panic!("expected JSON error ({err}): {output:?}"));
                assert_eq!(error["code"], "invalid_input", "{error}");
                let message = error["error"].as_str().unwrap_or("");
                assert!(
                    message.contains("orbit run task-pilot"),
                    "error must direct users to the dedicated command: {message}"
                );
                assert_eq!(
                    fixture.snapshot(),
                    before_runs,
                    "{subcommand:?} {target} {input_arg} must not create a durable job run"
                );
            }
        }
    }

    // Plain-mode refusal also exits 1 with empty stdout and actionable stderr.
    let plain_output = cli(&[
        "run",
        "job",
        "task_pilot_pipeline",
        "--input",
        &unquoted_bracket,
    ]);
    assert_eq!(plain_output.status.code(), Some(1), "{plain_output:?}");
    assert!(plain_output.stdout.is_empty(), "{plain_output:?}");
    let plain_stderr = String::from_utf8_lossy(&plain_output.stderr);
    assert!(plain_stderr.contains("error: "), "{plain_stderr}");
    assert!(
        plain_stderr.contains("orbit run task-pilot"),
        "{plain_stderr}"
    );
    assert_eq!(fixture.snapshot(), before_runs);

    // Malformed and empty task lists on the dedicated command also fail before creating runs.
    for bad_ids in ["", "[ORB-1]"] {
        let output = cli(&["run", "task-pilot", bad_ids, "--json"]);
        assert_eq!(output.status.code(), Some(1), "{bad_ids}: {output:?}");
        assert!(output.stdout.is_empty(), "{bad_ids}: {output:?}");
        assert_eq!(fixture.snapshot(), before_runs);
    }

    // Valid dedicated task-pilot invocation passes task_ids as a JSON array.
    let explicit = cli(&[
        "run",
        "task-pilot",
        &task.id,
        "--wait",
        "--timeout-seconds",
        "60",
        "--json",
    ]);
    assert!(explicit.status.success(), "{explicit:?}");
    let explicit_doc: Value = serde_json::from_slice(&explicit.stdout).unwrap();
    let explicit_run_id = explicit_doc["run_id"].as_str().unwrap();
    let explicit_run = runtime.show_job_run(explicit_run_id).unwrap();
    assert_eq!(
        explicit_run.input.as_ref().unwrap()["task_ids"],
        json!([task.id])
    );
    let explicit_state = runtime
        .read_run_state(explicit_run_id)
        .unwrap()
        .expect("explicit pilot run state");
    assert_eq!(
        explicit_state.pipeline["prepare"]["task_ids"],
        json!([task.id])
    );

    // Absent task list on dedicated task-pilot runs automatic discovery.
    let auto_pilot = cli(&[
        "run",
        "task-pilot",
        "--wait",
        "--timeout-seconds",
        "60",
        "--json",
    ]);
    assert!(auto_pilot.status.success(), "{auto_pilot:?}");
    let auto_doc: Value = serde_json::from_slice(&auto_pilot.stdout).unwrap();
    let auto_run = runtime
        .show_job_run(auto_doc["run_id"].as_str().unwrap())
        .unwrap();
    assert!(auto_run.input.as_ref().unwrap().get("task_ids").is_none());

    // Absent task_ids with unrelated generic key=value string inputs on task_pilot_pipeline works.
    let generic_pilot = cli(&[
        "run",
        "job",
        "task_pilot_pipeline",
        "--input",
        "base_branch=main",
        "--input",
        "max_tasks=1",
        "--wait",
        "--timeout-seconds",
        "60",
        "--json",
    ]);
    assert!(generic_pilot.status.success(), "{generic_pilot:?}");
    let generic_pilot_doc: Value = serde_json::from_slice(&generic_pilot.stdout).unwrap();
    let generic_pilot_run = runtime
        .show_job_run(generic_pilot_doc["run_id"].as_str().unwrap())
        .unwrap();
    assert_eq!(
        generic_pilot_run.input.as_ref().unwrap()["base_branch"],
        json!("main")
    );
    assert_eq!(
        generic_pilot_run.input.as_ref().unwrap()["max_tasks"],
        json!("1")
    );

    // Unrelated generic jobs retain prior Value::String interpretation even for `task_ids`.
    let generic_other = cli(&[
        "run",
        "job",
        "string_input_probe",
        "--input",
        "task_ids=plain-string-value",
        "--wait",
        "--timeout-seconds",
        "60",
        "--json",
    ]);
    assert!(generic_other.status.success(), "{generic_other:?}");
    let generic_other_doc: Value = serde_json::from_slice(&generic_other.stdout).unwrap();
    let generic_other_run = runtime
        .show_job_run(generic_other_doc["run_id"].as_str().unwrap())
        .unwrap();
    assert_eq!(
        generic_other_run.input.as_ref().unwrap()["task_ids"],
        json!("plain-string-value")
    );
}

#[test]
fn generic_job_run_validates_pilot_input_against_resolved_job_and_step() {
    if !isolated_run_observation(
        "run_observation::task_pilot_branch::generic_job_run_validates_pilot_input_against_resolved_job_and_step",
    ) {
        return;
    }
    let fixture = Fixture::terminal();
    git_repo::SeededRepo::new(&fixture.work).build();
    let origin = fixture.home.join("origin.git");
    git_repo::run(
        &fixture.work,
        &["clone", "--bare", ".", origin.to_str().unwrap()],
    );
    git_repo::run(
        &fixture.work,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    let runtime = fixture.registered_runtime();

    let activities_dir = fixture.work.join(".orbit/resources/activities");
    fs::create_dir_all(&activities_dir).unwrap();
    fs::write(
        activities_dir.join("custom_prepare.yaml"),
        json!({
            "schemaVersion": 2,
            "kind": "Activity",
            "metadata": {"name": "custom_prepare"},
            "spec": {
                "type": "deterministic",
                "description": "Custom-named prepare_task_pilot activity alias",
                "action": "prepare_task_pilot",
                "config": {}
            }
        })
        .to_string(),
    )
    .unwrap();

    // Direct non-pilot file named `task_pilot_pipeline` in the working directory
    // must win over the catalog name for both bare and `./` spellings on both aliases.
    let shadowed_file = fixture.work.join("task_pilot_pipeline");
    fs::write(
        &shadowed_file,
        json!({
            "schemaVersion": 2,
            "kind": "Job",
            "metadata": {"name": "string_input_probe"},
            "spec": {
                "state": "enabled",
                "kind": "workflow",
                "steps": [{
                    "id": "nap",
                    "default_input": {"seconds": 0, "note": "{{ input.task_ids }}"},
                    "spec": {"type": "deterministic", "action": "sleep", "config": {}}
                }]
            }
        })
        .to_string(),
    )
    .unwrap();

    // A direct file whose metadata.name is `task_pilot_pipeline` but has no
    // preparation step must also keep generic string `task_ids` input.
    let metadata_named_probe = fixture.home.join("metadata-pilot-name-probe.yaml");
    fs::write(
        &metadata_named_probe,
        json!({
            "schemaVersion": 2,
            "kind": "Job",
            "metadata": {"name": "task_pilot_pipeline"},
            "spec": {
                "state": "enabled",
                "kind": "workflow",
                "steps": [{
                    "id": "nap",
                    "default_input": {"seconds": 0, "note": "{{ input.task_ids }}"},
                    "spec": {"type": "deterministic", "action": "sleep", "config": {}}
                }]
            }
        })
        .to_string(),
    )
    .unwrap();

    // Custom job whose pilot step passes a literal `task_ids: []` array must
    // still permit a top-level string `task_ids` consumed by another step.
    fixture.write_job(
        "custom_pilot_literal_array.yaml",
        &json!({
            "schemaVersion": 2,
            "kind": "Job",
            "metadata": {"name": "custom_pilot_literal_array"},
            "spec": {
                "state": "enabled",
                "kind": "workflow",
                "steps": [
                    {
                        "id": "prepare",
                        "target": "activity:custom_prepare",
                        "default_input": {
                            "task_ids": [],
                            "workspace_path": ".",
                            "base_branch": "main"
                        }
                    },
                    {
                        "id": "probe",
                        "default_input": {"seconds": 0, "note": "{{ input.task_ids }}"},
                        "spec": {"type": "deterministic", "action": "sleep", "config": {}}
                    }
                ]
            }
        })
        .to_string(),
    );

    // Custom job whose pilot step wraps caller string `task_ids` into a JSON
    // string array template must succeed on both generic run aliases, keeping
    // the stored outer input as a string while preparation receives the array.
    fixture.write_job(
        "custom_pilot_array_template.yaml",
        &json!({
            "schemaVersion": 2,
            "kind": "Job",
            "metadata": {"name": "custom_pilot_array_template"},
            "spec": {
                "state": "enabled",
                "kind": "workflow",
                "steps": [{
                    "id": "prepare",
                    "target": "activity:custom_prepare",
                    "default_input": {
                        "task_ids": "[\"{{ input.task_ids }}\"]",
                        "workspace_path": ".",
                        "base_branch": "main"
                    }
                }]
            }
        })
        .to_string(),
    );
    fixture.write_job(
        "custom_pilot_json_object_array_template.yaml",
        &json!({
            "schemaVersion": 2,
            "kind": "Job",
            "metadata": {"name": "custom_pilot_json_object_array_template"},
            "spec": {
                "state": "enabled",
                "kind": "workflow",
                "steps": [{
                    "id": "prepare",
                    "target": "activity:custom_prepare",
                    "default_input": "{\"task_ids\":[{{ input.task_ids }}],\"workspace_path\":\".\",\"base_branch\":\"main\"}"
                }]
            }
        })
        .to_string(),
    );

    // Custom jobs whose `activity:custom_prepare` step forwards caller string
    // `task_ids` (explicitly, via a JSON-object template, or via omitted/whole-input
    // default_input) must fail with `invalid_input` before any durable run exists.
    fixture.write_job(
        "aliased_pilot_forwarded.yaml",
        &json!({
            "schemaVersion": 2,
            "kind": "Job",
            "metadata": {"name": "aliased_pilot_forwarded"},
            "spec": {
                "state": "enabled",
                "kind": "workflow",
                "steps": [{
                    "id": "prepare",
                    "target": "activity:custom_prepare",
                    "default_input": {
                        "task_ids": "{{ input.task_ids }}",
                        "workspace_path": "."
                    }
                }]
            }
        })
        .to_string(),
    );
    fixture.write_job(
        "custom_pilot_json_object_string.yaml",
        &json!({
            "schemaVersion": 2,
            "kind": "Job",
            "metadata": {"name": "custom_pilot_json_object_string"},
            "spec": {
                "state": "enabled",
                "kind": "workflow",
                "steps": [{
                    "id": "prepare",
                    "target": "activity:custom_prepare",
                    "default_input": "{\"task_ids\":\"{{ input.task_ids }}\",\"workspace_path\":\".\",\"base_branch\":\"main\"}"
                }]
            }
        })
        .to_string(),
    );
    fixture.write_job(
        "custom_pilot_json_object_bare.yaml",
        &json!({
            "schemaVersion": 2,
            "kind": "Job",
            "metadata": {"name": "custom_pilot_json_object_bare"},
            "spec": {
                "state": "enabled",
                "kind": "workflow",
                "steps": [{
                    "id": "prepare",
                    "target": "activity:custom_prepare",
                    "default_input": "{\"task_ids\":{{ input.task_ids }},\"workspace_path\":\".\",\"base_branch\":\"main\"}"
                }]
            }
        })
        .to_string(),
    );
    let aliased_whole_input_path = fixture.home.join("aliased-whole-input.yaml");
    fs::write(
        &aliased_whole_input_path,
        json!({
            "schemaVersion": 2,
            "kind": "Job",
            "metadata": {"name": "aliased_whole_input"},
            "spec": {
                "state": "enabled",
                "kind": "workflow",
                "steps": [{
                    "id": "prepare",
                    "target": "activity:custom_prepare",
                    "default_input": "{{ input }}"
                }]
            }
        })
        .to_string(),
    )
    .unwrap();

    let cli = |args: &[&str]| {
        let output = fixture
            .orbit()
            .env("PATH", std::env::var_os("PATH").unwrap())
            .args(args)
            .timeout(Duration::from_secs(120))
            .output()
            .unwrap();
        if let Ok(result) = serde_json::from_slice::<Value>(&output.stdout)
            && let Some(id) = result["run_id"].as_str()
        {
            let run = runtime.show_job_run(id).unwrap();
            test_env::wait_until("disposable pilot worker exit", || {
                run.pid.is_none_or(|pid| {
                    probe_process_liveness(pid, run.pid_start_time.as_deref())
                        == ProcessLiveness::Exited
                })
            });
        }
        output
    };

    for subcommand in [["run", "job"], ["job", "run"]] {
        for target in ["task_pilot_pipeline", "./task_pilot_pipeline"] {
            let output = cli(&[
                subcommand[0],
                subcommand[1],
                target,
                "--input",
                "task_ids=plain-string-value",
                "--wait",
                "--timeout-seconds",
                "60",
                "--json",
            ]);
            assert!(
                output.status.success(),
                "{subcommand:?} {target} must select direct non-pilot file: {output:?}"
            );
            let doc: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(doc["job_id"], "string_input_probe", "{doc}");
            let run = runtime
                .show_job_run(doc["run_id"].as_str().unwrap())
                .unwrap();
            assert_eq!(
                run.input.as_ref().unwrap()["task_ids"],
                json!("plain-string-value")
            );
        }
    }

    let meta_probe_output = cli(&[
        "run",
        "job",
        metadata_named_probe.to_str().unwrap(),
        "--input",
        "task_ids=plain-string-value",
        "--wait",
        "--timeout-seconds",
        "60",
        "--json",
    ]);
    assert!(meta_probe_output.status.success(), "{meta_probe_output:?}");
    let meta_probe_doc: Value = serde_json::from_slice(&meta_probe_output.stdout).unwrap();
    let meta_probe_run = runtime
        .show_job_run(meta_probe_doc["run_id"].as_str().unwrap())
        .unwrap();
    assert_eq!(
        meta_probe_run.input.as_ref().unwrap()["task_ids"],
        json!("plain-string-value")
    );

    // Once the local file is absent, bare `task_pilot_pipeline` resolves to the
    // shipped catalog pilot again and refuses string `task_ids`.
    fs::remove_file(&shadowed_file).unwrap();

    for subcommand in [["run", "job"], ["job", "run"]] {
        let output = cli(&[
            subcommand[0],
            subcommand[1],
            "custom_pilot_literal_array",
            "--input",
            "task_ids=plain-string-value",
            "--wait",
            "--timeout-seconds",
            "60",
            "--json",
        ]);
        assert!(
            output.status.success(),
            "{subcommand:?} custom_pilot_literal_array must allow top-level string task_ids: {output:?}"
        );
        let doc: Value = serde_json::from_slice(&output.stdout).unwrap();
        let run_id = doc["run_id"].as_str().unwrap();
        let run = runtime.show_job_run(run_id).unwrap();
        assert_eq!(
            run.input.as_ref().unwrap()["task_ids"],
            json!("plain-string-value")
        );
        let state = runtime.read_run_state(run_id).unwrap().unwrap();
        assert_eq!(state.pipeline["prepare"]["mode"], json!("automatic"));
        assert_eq!(state.pipeline["prepare"]["task_ids"], json!([]));
    }

    let task = runtime
        .add_task(TaskAddParams {
            title: "Custom pilot template rendering fixture".into(),
            acceptance_criteria: vec!["Render caller string task_ids into an array".into()],
            tags: vec!["delivery:task_local_pipeline".into()],
            ..Default::default()
        })
        .unwrap();
    let task_id_input = format!("task_ids={}", task.id);
    for subcommand in [["run", "job"], ["job", "run"]] {
        let output = cli(&[
            subcommand[0],
            subcommand[1],
            "custom_pilot_array_template",
            "--input",
            &task_id_input,
            "--wait",
            "--timeout-seconds",
            "60",
            "--json",
        ]);
        assert!(
            output.status.success(),
            "{subcommand:?} custom_pilot_array_template must accept string task_ids rendered into a string array: {output:?}"
        );
        let doc: Value = serde_json::from_slice(&output.stdout).unwrap();
        let run_id = doc["run_id"].as_str().unwrap();
        let run = runtime.show_job_run(run_id).unwrap();
        assert_eq!(
            run.input.as_ref().unwrap()["task_ids"],
            json!(task.id),
            "stored outer input must remain a string"
        );
        let state = runtime.read_run_state(run_id).unwrap().unwrap();
        assert_eq!(state.pipeline["prepare"]["mode"], json!("explicit"));
        assert_eq!(state.pipeline["prepare"]["task_ids"], json!([task.id]));
    }

    let quoted_task_id = format!("\"{}\"", task.id);
    let quoted_task_id_input = format!("task_ids={quoted_task_id}");
    for subcommand in [["run", "job"], ["job", "run"]] {
        let output = cli(&[
            subcommand[0],
            subcommand[1],
            "custom_pilot_json_object_array_template",
            "--input",
            &quoted_task_id_input,
            "--wait",
            "--timeout-seconds",
            "60",
            "--json",
        ]);
        assert!(
            output.status.success(),
            "{subcommand:?} custom_pilot_json_object_array_template must accept quoted JSON task_ids fragment: {output:?}"
        );
        let doc: Value = serde_json::from_slice(&output.stdout).unwrap();
        let run_id = doc["run_id"].as_str().unwrap();
        let run = runtime.show_job_run(run_id).unwrap();
        assert_eq!(
            run.input.as_ref().unwrap()["task_ids"],
            json!(quoted_task_id),
            "stored outer input must preserve the caller quotes"
        );
        let state = runtime.read_run_state(run_id).unwrap().unwrap();
        assert_eq!(state.pipeline["prepare"]["mode"], json!("explicit"));
        assert_eq!(state.pipeline["prepare"]["task_ids"], json!([task.id]));
    }

    let before_refusals = fixture.snapshot();
    let json_array_string = format!("task_ids=[\"{}\"]", task.id);
    for (target, input_arg) in [
        ("task_pilot_pipeline", "task_ids=plain-string-value"),
        ("task_pilot_pipeline", json_array_string.as_str()),
        ("task_pilot_pipeline", "task_ids=[]"),
        ("aliased_pilot_forwarded", "task_ids=plain-string-value"),
        ("aliased_pilot_forwarded", json_array_string.as_str()),
        ("custom_pilot_json_object_string", task_id_input.as_str()),
        ("custom_pilot_json_object_bare", json_array_string.as_str()),
        (
            "custom_pilot_json_object_array_template",
            task_id_input.as_str(),
        ),
        (
            "custom_pilot_json_object_array_template",
            json_array_string.as_str(),
        ),
        (
            aliased_whole_input_path.to_str().unwrap(),
            "task_ids=plain-string-value",
        ),
    ] {
        for subcommand in [["run", "job"], ["job", "run"]] {
            let output = cli(&[
                subcommand[0],
                subcommand[1],
                target,
                "--input",
                input_arg,
                "--json",
            ]);
            assert_eq!(
                output.status.code(),
                Some(1),
                "{subcommand:?} {target} {input_arg} must fail before submit: {output:?}"
            );
            assert!(output.stdout.is_empty(), "{output:?}");
            let error: Value = serde_json::from_slice(&output.stderr)
                .unwrap_or_else(|err| panic!("expected JSON error ({err}): {output:?}"));
            assert_eq!(error["code"], "invalid_input", "{error}");
            let message = error["error"].as_str().unwrap_or("");
            assert!(
                message.contains("orbit run task-pilot"),
                "error must retain actionable remedy: {message}"
            );
            assert_eq!(
                fixture.snapshot(),
                before_refusals,
                "{subcommand:?} {target} {input_arg} must not persist a durable run"
            );
        }
    }

    // Guarded `activity:custom_prepare` steps (and guarded enclosing blocks)
    // whose input-only `when` evaluates to false must skip without rejecting
    // unrelated caller string `task_ids`, letting later steps consume it.
    fixture.write_job(
        "custom_pilot_guarded_prepare.yaml",
        &json!({
            "schemaVersion": 2,
            "kind": "Job",
            "metadata": {"name": "custom_pilot_guarded_prepare"},
            "spec": {
                "state": "enabled",
                "kind": "workflow",
                "steps": [
                    {
                        "id": "prepare",
                        "when": "{{ input.mode }} == pilot",
                        "target": "activity:custom_prepare",
                        "default_input": "{{ input }}"
                    },
                    {
                        "id": "probe",
                        "default_input": {"seconds": 0, "note": "{{ input.task_ids }}"},
                        "spec": {"type": "deterministic", "action": "sleep", "config": {}}
                    }
                ]
            }
        })
        .to_string(),
    );
    fixture.write_job(
        "custom_pilot_guarded_block.yaml",
        &json!({
            "schemaVersion": 2,
            "kind": "Job",
            "metadata": {"name": "custom_pilot_guarded_block"},
            "spec": {
                "state": "enabled",
                "kind": "workflow",
                "steps": [
                    {
                        "id": "guarded_prepare",
                        "when": "{{ input.mode }} == pilot",
                        "parallel": {
                            "join": {"mode": "all"},
                            "branches": [{
                                "id": "prepare",
                                "target": "activity:custom_prepare",
                                "default_input": "{{ input }}"
                            }]
                        }
                    },
                    {
                        "id": "probe",
                        "default_input": {"seconds": 0, "note": "{{ input.task_ids }}"},
                        "spec": {"type": "deterministic", "action": "sleep", "config": {}}
                    }
                ]
            }
        })
        .to_string(),
    );

    for target in ["custom_pilot_guarded_prepare", "custom_pilot_guarded_block"] {
        for subcommand in [["run", "job"], ["job", "run"]] {
            let output = cli(&[
                subcommand[0],
                subcommand[1],
                target,
                "--input",
                "mode=probe",
                "--input",
                "task_ids=plain-string-value",
                "--wait",
                "--timeout-seconds",
                "60",
                "--json",
            ]);
            assert!(
                output.status.success(),
                "{subcommand:?} {target} with false preparation guard must run probe: {output:?}"
            );
            let doc: Value = serde_json::from_slice(&output.stdout).unwrap();
            let run_id = doc["run_id"].as_str().unwrap();
            let run = runtime.show_job_run(run_id).unwrap();
            assert_eq!(
                run.input.as_ref().unwrap()["task_ids"],
                json!("plain-string-value")
            );
            let state = runtime.read_run_state(run_id).unwrap().unwrap();
            assert!(
                state.pipeline.get("prepare").is_none(),
                "{subcommand:?} {target} must record no preparation output: {state:?}"
            );
            assert!(
                state.pipeline.get("guarded_prepare").is_none(),
                "{subcommand:?} {target} must record no skipped block output: {state:?}"
            );
            assert!(
                state.pipeline["probe"]["slept_seconds"].is_number(),
                "{subcommand:?} {target} probe step must run: {state:?}"
            );
        }
    }

    let before_guarded_refusals = fixture.snapshot();
    for target in ["custom_pilot_guarded_prepare", "custom_pilot_guarded_block"] {
        for subcommand in [["run", "job"], ["job", "run"]] {
            let output = cli(&[
                subcommand[0],
                subcommand[1],
                target,
                "--input",
                "mode=pilot",
                "--input",
                "task_ids=plain-string-value",
                "--json",
            ]);
            assert_eq!(
                output.status.code(),
                Some(1),
                "{subcommand:?} {target} with true preparation guard must fail before submit: {output:?}"
            );
            assert!(output.stdout.is_empty(), "{output:?}");
            let error: Value = serde_json::from_slice(&output.stderr)
                .unwrap_or_else(|err| panic!("expected JSON error ({err}): {output:?}"));
            assert_eq!(error["code"], "invalid_input", "{error}");
            let message = error["error"].as_str().unwrap_or("");
            assert!(
                message.contains("orbit run task-pilot"),
                "error must direct users to the dedicated command: {message}"
            );
            assert_eq!(
                fixture.snapshot(),
                before_guarded_refusals,
                "{subcommand:?} {target} must not persist a durable run when guard is true"
            );
        }
    }
}
