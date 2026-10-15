//! A PR-mode workspace whose Git remotes name no network host is refused at
//! admission — explicit ship, the drain's backlog and readiness, and
//! task-pilot preparation — before any run, worktree or crew exists. A
//! `delivery:task_local_pipeline` tag delivers that one task locally instead.

use std::path::{Path, PathBuf};

use orbit_core::application::job::JobRunListParams;
use orbit_core::application::task::TaskAddParams;
use orbit_core::{
    CompletionPolicy, OrbitError, OrbitRuntime, ShipMode, TaskComplexity, TaskStatus,
    WorkspaceRuntimeBinding,
};
use orbit_engine::RuntimeHost;
use orbit_tools::ToolContext;
use orbit_types::workflow::JobRunTrigger;
use serde_json::{Value, json};
use tempfile::TempDir;

const LOCAL_TAG: &str = "delivery:task_local_pipeline";

/// A workspace registered for PR delivery whose only remote, `origin`, is a
/// local bare repository — the shape a git server moved onto the same box
/// leaves behind.
struct Forgeless {
    _root: TempDir,
    runtime: OrbitRuntime,
    repo: PathBuf,
    bare: PathBuf,
}

fn open() -> Forgeless {
    let root = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("home/.orbit");
    let repo = root.path().join("repo");
    let bare = root.path().join("constellation.git");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(repo.join(".orbit")).unwrap();
    git(&repo, &["init", "-b", "main"]);
    git(&repo, &["config", "user.name", "Orbit Test"]);
    git(&repo, &["config", "user.email", "orbit-test@example.com"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    std::fs::write(repo.join(".gitignore"), ".orbit/\n").unwrap();
    std::fs::write(repo.join("README.md"), "fixture\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "seed"]);
    git(root.path(), &["init", "--bare", bare.to_str().unwrap()]);
    git(&repo, &["remote", "add", "origin", bare.to_str().unwrap()]);
    git(&repo, &["push", "origin", "main"]);
    let binding = WorkspaceRuntimeBinding {
        logical_workspace_id: "ws_forgeless".to_string(),
        task_partition_id: "ws_forgeless".to_string(),
        owner_machine_id: None,
        checkout_role: None,
        repo_root: repo.clone(),
        ship_mode: ShipMode::Pr,
        base_branch: Some("main".to_string()),
    };
    let runtime = OrbitRuntime::from_roots_with_binding(&global, &repo.join(".orbit"), binding)
        .unwrap()
        .with_host_resource_probe(super::dispatch_admission::PressureProbe::calm());
    Forgeless {
        _root: root,
        runtime,
        repo,
        bare,
    }
}

fn git(cwd: &Path, args: &[&str]) -> String {
    let mut command = crate::git_test_support::command();
    let output = command.args(args).current_dir(cwd).output().unwrap();
    assert!(
        output.status.success(),
        "git {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn seed(runtime: &OrbitRuntime, title: &str, tags: &[&str]) -> String {
    runtime
        .add_task(TaskAddParams {
            title: title.to_string(),
            description: format!("Fixture task: {title}"),
            acceptance_criteria: vec!["The task is observable.".to_string()],
            plan: "Fixture plan.".to_string(),
            tags: tags.iter().map(ToString::to_string).collect(),
            complexity: TaskComplexity::Medium,
            context_files: vec!["file:README.md".into()],
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .expect("seed task")
        .id
        .to_string()
}

/// An explicit PR ship of `task`. The fixture deploys no job asset, so a
/// task admission lets through fails next, on the missing asset.
fn ship(runtime: &OrbitRuntime, task: &str) -> OrbitError {
    runtime
        .submit_ship_run(
            ShipMode::Pr,
            Some("main"),
            &[task.to_string()],
            CompletionPolicy::Review,
            &[],
            Some("test"),
            None,
            JobRunTrigger::cli(),
        )
        .expect_err("a fixture without job assets never submits a run")
}

fn action(runtime: &OrbitRuntime, action: &str, input: Value) -> Value {
    runtime
        .run_deterministic(action, &json!({}), &input, ToolContext::default())
        .unwrap_or_else(|error| panic!("{action}: {error}"))
}

fn excluded<'a>(output: &'a Value, field: &str, task: &str) -> Option<&'a Value> {
    output["excluded"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry[field] == task)
}

fn readiness_entry(runtime: &OrbitRuntime, task: &str) -> Value {
    let readiness = runtime
        .workspace_auto_readiness(&[], None, 50, &[])
        .unwrap();
    readiness["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["task_id"] == task)
        .cloned()
        .unwrap_or_else(|| panic!("{task} missing from readiness: {readiness}"))
}

/// The refusal names the remote it found and both ways out.
fn assert_names_remote_and_fixes(message: &str, bare: &Path) {
    for expected in [
        bare.to_str().unwrap(),
        "orbit workspace ship-mode local",
        LOCAL_TAG,
    ] {
        assert!(message.contains(expected), "missing {expected}: {message}");
    }
}

/// Explicit ship, the drain's backlog and readiness refuse an untagged task
/// with the typed refusal before any run or worktree exists, and admit the
/// tagged one. A remote on a network host lifts the refusal.
fn a_pr_workspace_without_a_forge_remote_refuses_untagged_work_before_any_run() {
    let fixture = open();
    let runtime = &fixture.runtime;
    let plain = seed(runtime, "plain", &[]);
    let local = seed(runtime, "local", &[LOCAL_TAG]);

    let refused = ship(runtime, &plain);
    assert!(
        matches!(refused, OrbitError::PrForgeRemoteMissing { .. }),
        "{refused:?}"
    );
    assert_names_remote_and_fixes(&refused.to_string(), &fixture.bare);
    let admitted = ship(runtime, &local);
    assert!(
        matches!(admitted, OrbitError::NotFound { .. }),
        "a tagged task passes admission and reaches the job load: {admitted:?}"
    );
    let runs = runtime.list_job_runs(JobRunListParams::default()).unwrap();
    assert!(runs.is_empty(), "no run may exist: {runs:#?}");
    let worktrees = git(&fixture.repo, &["worktree", "list", "--porcelain"]);
    assert_eq!(
        worktrees.matches("worktree ").count(),
        1,
        "no worktree may exist: {worktrees}"
    );

    let backlog = action(runtime, "list_backlog_tasks", json!({}));
    assert_eq!(backlog["task_ids"], json!([local]), "{backlog}");
    let held = excluded(&backlog, "id", &plain).expect("the plain task is withheld");
    assert_eq!(held["reason"], "pr_forge_remote_missing", "{backlog}");
    assert_names_remote_and_fixes(held["detail"].as_str().unwrap(), &fixture.bare);
    let entry = readiness_entry(runtime, &plain);
    assert_eq!(entry["eligible"], false, "{entry}");
    assert_eq!(entry["reason"], "pr_forge_remote_missing", "{entry}");
    assert_eq!(readiness_entry(runtime, &local)["eligible"], true);

    // Any network host is a forge candidate: `gh` alone knows enterprise
    // hosts and SSH aliases, so admission only refuses a set that has none.
    git(
        &fixture.repo,
        &[
            "remote",
            "add",
            "upstream",
            "https://github.com/example/forgeless.git",
        ],
    );
    let backlog = action(runtime, "list_backlog_tasks", json!({}));
    assert_eq!(backlog["task_ids"], json!([plain, local]), "{backlog}");
}

/// Task-pilot preparation does not stage work the PR pipeline could never
/// open a pull request for, and reports the refusal once.
fn pilot_preparation_skips_work_a_forgeless_pr_workspace_cannot_deliver() {
    let fixture = open();
    let runtime = &fixture.runtime;
    let plain = seed(runtime, "plain", &[]);
    let local = seed(runtime, "local", &[LOCAL_TAG]);

    let prepared = action(
        runtime,
        "prepare_task_pilot",
        json!({
            "task_ids": [plain, local],
            "workspace_path": fixture.repo,
            "base_branch": "main",
        }),
    );
    assert_eq!(prepared["task_ids"], json!([local]), "{prepared}");
    let skipped = excluded(&prepared, "task_id", &plain).expect("the plain task is skipped");
    assert_eq!(skipped["reason"], "pr_forge_remote_missing", "{prepared}");
    assert_names_remote_and_fixes(
        prepared["pr_forge_refusal"].as_str().unwrap(),
        &fixture.bare,
    );
}

/// A bundled PR must fail at the gate's public route resolver, before a
/// reservation or worker can exist. The same input remains valid locally.
fn multi_task_pr_is_refused_before_gate_reservation() {
    let fixture = open();
    let runtime = &fixture.runtime;
    let first = seed(runtime, "first", &[]);
    let second = seed(runtime, "second", &[]);
    git(
        &fixture.repo,
        &[
            "remote",
            "add",
            "upstream",
            "https://github.com/example/repo.git",
        ],
    );
    let tasks = [first, second];
    let refused = runtime
        .run_deterministic(
            "resolve_delivery_job",
            &json!({}),
            &json!({"task_ids": tasks, "mode": "pr"}),
            ToolContext::default(),
        )
        .expect_err("multi-task PR delivery must refuse before reserving locks");
    assert!(
        refused.to_string().contains("exactly one distinct task"),
        "{refused}"
    );
    assert!(refused.to_string().contains("local"), "{refused}");
    let local = action(
        runtime,
        "resolve_delivery_job",
        json!({"task_ids": tasks, "mode": "local"}),
    );
    assert_eq!(local["job_name"], "task_local_pipeline");
    let duplicate = action(
        runtime,
        "resolve_delivery_job",
        json!({"task_ids": [tasks[0], tasks[0]], "mode": "pr"}),
    );
    assert_eq!(duplicate["job_name"], "task_pr_pipeline");
    for id in tasks {
        let task = runtime.get_task(&id).unwrap();
        assert_eq!(task.status, TaskStatus::Backlog);
        assert!(task.job_run_id.is_none());
    }
    assert!(
        runtime
            .list_job_runs(JobRunListParams::default())
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        git(&fixture.repo, &["worktree", "list", "--porcelain"])
            .matches("worktree ")
            .count(),
        1
    );
}

/// The shared submission API rejects bundled PR leaves regardless of catalog
/// name or snapshot origin. A deliberately unavailable worker makes accepted
/// controls stop at startup, so the test never launches a provider or leaves a
/// detached process alive.
#[test]
fn bundled_pr_submission_refuses_before_any_delivery_effect() {
    if !super::dispatch_admission::isolated(
        "pr_forge_admission::bundled_pr_submission_refuses_before_any_delivery_effect",
    ) {
        return;
    }
    let fixture = open();
    let runtime = &fixture.runtime;
    orbit_core::bootstrap::init::init_workspace_at_root(
        &runtime.global_root(),
        orbit_core::bootstrap::init::InitOptions {
            global_only: true,
            ..Default::default()
        },
    )
    .unwrap();
    orbit_core::test_support::install_substitute_pipeline_worker([fixture
        ._root
        .path()
        .join("worker-must-not-start")
        .to_string_lossy()
        .into_owned()]);
    git(
        &fixture.repo,
        &[
            "remote",
            "add",
            "upstream",
            "https://github.com/example/repo.git",
        ],
    );
    let ids = [seed(runtime, "first", &[]), seed(runtime, "second", &[])];
    let before_tasks: Vec<_> = ids
        .iter()
        .map(|id| serde_json::to_value(runtime.get_task(id).unwrap()).unwrap())
        .collect();
    let before_git = git(&fixture.repo, &["show-ref"]);
    let before_index = git(&fixture.repo, &["status", "--porcelain=v1"]);
    let before_worktrees = git(&fixture.repo, &["worktree", "list", "--porcelain"]);

    // Preserve a prior candidate and its durable checkpoint through every refusal.
    let retained = fixture._root.path().join("retained");
    git(
        &fixture.repo,
        &[
            "worktree",
            "add",
            "--detach",
            retained.to_str().unwrap(),
            "HEAD",
        ],
    );
    std::fs::write(retained.join("candidate.txt"), "retained candidate").unwrap();
    let jobs = orbit_store::compose::workspace_job_run_store(
        runtime.sqlite_store().unwrap(),
        runtime.workspace_id().unwrap(),
    );
    let parent = jobs
        .insert_job_run(
            "task_gate_pipeline",
            1,
            chrono::Utc::now(),
            Some(json!({})),
            None,
        )
        .unwrap();
    let checkpoint = orbit_types::workflow::PipelineState::new(
        parent.run_id.clone(),
        parent.job_id.clone(),
        json!({"retained": true}),
    );
    jobs.write_run_state(&parent.run_id, &checkpoint).unwrap();
    let saved_checkpoint =
        serde_json::to_value(jobs.read_run_state(&parent.run_id).unwrap()).unwrap();
    let saved_runs =
        serde_json::to_value(runtime.list_job_runs(JobRunListParams::default()).unwrap()).unwrap();

    let yaml = std::fs::read_to_string(
        runtime
            .global_root()
            .join("resources/jobs/task_pr_pipeline.yaml"),
    )
    .unwrap();
    let copy = fixture._root.path().join("renamed-pr.yaml");
    std::fs::write(
        &copy,
        yaml.replace("name: task_pr_pipeline", "name: renamed_pr"),
    )
    .unwrap();
    let input = json!({"task_ids": ids});
    for job in ["task_pr_pipeline", copy.to_str().unwrap()] {
        let error = runtime
            .submit_job_run(job, input.clone(), Some("test"))
            .unwrap_err();
        assert!(
            error.to_string().contains("exactly one distinct task"),
            "{job}: {error}"
        );
        assert!(error.to_string().contains("singleton"), "{error}");
        assert!(error.to_string().contains("local"), "{error}");
    }
    // A selected catalog copy must also refuse at the gate before reservation.
    std::fs::write(
        runtime.global_root().join("resources/jobs/renamed_pr.yaml"),
        std::fs::read(&copy).unwrap(),
    )
    .unwrap();
    let tagged = [
        seed(runtime, "selected first", &["delivery:renamed_pr"]),
        seed(runtime, "selected second", &["delivery:renamed_pr"]),
    ];
    let selected_error = runtime
        .run_deterministic(
            "resolve_delivery_job",
            &json!({}),
            &json!({"task_ids": tagged, "mode": "pr"}),
            ToolContext::default(),
        )
        .unwrap_err();
    assert!(
        selected_error
            .to_string()
            .contains("exactly one distinct task"),
        "{selected_error}"
    );
    let local_tagged = [
        seed(runtime, "local first", &[LOCAL_TAG]),
        seed(runtime, "local second", &[LOCAL_TAG]),
    ];
    assert_eq!(
        action(
            runtime,
            "resolve_delivery_job",
            json!({"task_ids": local_tagged, "mode": "pr"})
        )["job_name"],
        "task_local_pipeline"
    );

    // A snapshot's default selection is as authoritative as explicit input.
    let mut defaulted: Value = serde_yaml::from_str(&yaml).unwrap();
    defaulted["metadata"]["name"] = json!("defaulted_pr");
    defaulted["spec"]["default_input"]["task_ids"] = json!(ids);
    let default_copy = fixture._root.path().join("defaulted-pr.yaml");
    std::fs::write(&default_copy, serde_yaml::to_string(&defaulted).unwrap()).unwrap();
    let error = runtime
        .submit_job_run(default_copy.to_str().unwrap(), json!({}), None)
        .unwrap_err();
    assert!(
        error.to_string().contains("exactly one distinct task"),
        "{error}"
    );

    // Inline and nested action forms keep the same singleton commit contract.
    let mut inline: Value = serde_yaml::from_str(&yaml).unwrap();
    inline["metadata"]["name"] = json!("inline_pr");
    let steps = inline["spec"]["steps"].as_array_mut().unwrap();
    let commit = steps
        .iter_mut()
        .find(|step| step["id"] == "commit")
        .unwrap();
    commit.as_object_mut().unwrap().remove("target");
    commit["spec"] = json!({"type": "deterministic", "action": "git_commit", "config": {}});
    *commit = json!({"id": "commit_wrapper", "parallel": {"join": {"mode": "all"}, "branches": [commit.clone()]}});
    let inline_copy = fixture._root.path().join("inline-pr.yaml");
    std::fs::write(&inline_copy, serde_yaml::to_string(&inline).unwrap()).unwrap();
    let error = runtime
        .submit_job_run(inline_copy.to_str().unwrap(), input.clone(), None)
        .unwrap_err();
    assert!(
        error.to_string().contains("exactly one distinct task"),
        "{error}"
    );

    // The public foreground API also refuses before inserting a run. An
    // executable fail-fast first step prevents worktree/provider effects if
    // this regression ever loses its admission check.
    for hook in [
        "recovery_activity",
        "final_recovery_activity",
        "failure_activity",
    ] {
        inline["spec"].as_object_mut().unwrap().remove(hook);
    }
    inline["spec"]["steps"].as_array_mut().unwrap().insert(0, json!({
        "id": "safe_stop", "spec": {"type": "deterministic", "action": "pipeline_success_guard", "config": {}},
        "default_input": {"results": [{"status": "failed"}]}
    }));
    std::fs::write(&inline_copy, serde_yaml::to_string(&inline).unwrap()).unwrap();
    let foreground = runtime
        .run_job_v2_from_yaml(&inline_copy, input.clone())
        .unwrap_err();
    assert!(
        foreground.to_string().contains("exactly one distinct task"),
        "{foreground}"
    );

    let child = runtime.run_deterministic(
        "invoke_and_wait", &json!({}),
        &json!({"job_name": "task_pr_pipeline", "run_input": input, "run_id": parent.run_id, "step_id": "deliver", "timeout_seconds": 1}),
        ToolContext {
            session_context: orbit_types::tool::ToolSessionContext {
                effective_capabilities: std::collections::BTreeSet::from([orbit_types::tool::McpCapability::Operator]),
                ..Default::default()
            }, ..Default::default()
        },
    ).unwrap_err();
    assert!(
        child.to_string().contains("exactly one distinct task"),
        "{child}"
    );
    assert_eq!(
        serde_json::to_value(runtime.list_job_runs(JobRunListParams::default()).unwrap()).unwrap(),
        saved_runs
    );
    assert_eq!(
        serde_json::to_value(jobs.read_run_state(&parent.run_id).unwrap()).unwrap(),
        saved_checkpoint
    );
    assert!(runtime.list_task_reservation_history().unwrap().is_empty());
    assert_eq!(git(&fixture.repo, &["show-ref"]), before_git);
    assert_eq!(
        git(&fixture.repo, &["status", "--porcelain=v1"]),
        before_index
    );
    assert_eq!(
        std::fs::read_to_string(retained.join("candidate.txt")).unwrap(),
        "retained candidate"
    );
    git(
        &fixture.repo,
        &["worktree", "remove", "--force", retained.to_str().unwrap()],
    );
    assert_eq!(
        git(&fixture.repo, &["worktree", "list", "--porcelain"]),
        before_worktrees
    );
    for (id, before) in ids.iter().zip(before_tasks) {
        assert_eq!(
            serde_json::to_value(runtime.get_task(id).unwrap()).unwrap(),
            before
        );
    }

    // Ship selects several tasks for separate singleton leaves; its coordinator
    // remains admissible and does not bind either task during submission.
    let ship_error = runtime
        .submit_ship_run(
            ShipMode::Pr,
            Some("main"),
            &ids,
            CompletionPolicy::Review,
            &[],
            None,
            None,
            JobRunTrigger::cli(),
        )
        .unwrap_err();
    assert!(
        ship_error.to_string().contains("spawn pipeline worker"),
        "{ship_error}"
    );
    for id in &ids {
        let task = runtime.get_task(id).unwrap();
        assert_eq!(task.status, TaskStatus::Backlog);
        assert!(task.job_run_id.is_none());
    }
    let unrelated = fixture._root.path().join("array-job.yaml");
    std::fs::write(&unrelated, serde_json::to_string(&json!({
        "schemaVersion": 2, "kind": "Job", "metadata": {"name": "array_job"},
        "spec": {"state": "enabled", "steps": [{"id": "check", "spec": {"type": "deterministic", "action": "pipeline_success_guard", "config": {}}}]}
    })).unwrap()).unwrap();
    let unrelated_error = runtime
        .submit_job_run(unrelated.to_str().unwrap(), input.clone(), None)
        .unwrap_err();
    assert!(
        unrelated_error
            .to_string()
            .contains("spawn pipeline worker"),
        "{unrelated_error}"
    );
    assert_eq!(
        jobs.list_job_runs("array_job").unwrap()[0]
            .input
            .as_ref()
            .unwrap()["task_ids"],
        json!(ids)
    );

    // Accepted controls reach worker startup, where the unavailable executable
    // fails deterministically without a process. Their persisted inputs prove
    // cardinality validation did not globally reject arrays or widen authority.
    for (job, tasks) in [
        ("task_pr_pipeline", json!([ids[0], ids[0]])),
        ("task_local_pipeline", json!(ids)),
        ("task_auto_pipeline", json!(ids)),
    ] {
        let error = runtime
            .submit_job_run(
                job,
                json!({"task_ids": tasks, "mode": "pr", "completion": "review", "auto_fix": false}),
                None,
            )
            .unwrap_err();
        assert!(
            error.to_string().contains("spawn pipeline worker"),
            "{job}: {error}"
        );
        let runs = jobs.list_job_runs(job).unwrap();
        let admitted = runs
            .first()
            .expect("accepted control persists a run before worker startup");
        let expected = if job == "task_pr_pipeline" {
            json!([ids[0]])
        } else {
            json!(ids)
        };
        assert_eq!(admitted.input.as_ref().unwrap()["task_ids"], expected);
        assert_eq!(admitted.input.as_ref().unwrap()["completion"], "review");
        assert_eq!(admitted.input.as_ref().unwrap()["auto_fix"], false);
    }
}

// Related boundary scenarios share the isolated table fixture.
crate::scenario_table!(
    a_pr_workspace_without_a_forge_remote_refuses_untagged_work_before_any_run_scenarios,
    a_pr_workspace_without_a_forge_remote_refuses_untagged_work_before_any_run,
    pilot_preparation_skips_work_a_forgeless_pr_workspace_cannot_deliver,
    multi_task_pr_is_refused_before_gate_reservation
);
