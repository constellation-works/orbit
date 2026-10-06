//! `os:` tags route a task to a host of that OS [ORB-14005].
//!
//! Local admission — the drain's wave, ship discovery, readiness and an
//! explicit ship — reads a task's `os:` tags against the runtime's host OS.
//! The fixture composes the host OS, so each test behaves the same on the
//! Linux and macOS machines CI runs it on.

use orbit_core::application::task::TaskAddParams;
use orbit_core::{
    CompletionPolicy, OrbitError, OrbitRuntime, ShipMode, Task, TaskComplexity, TaskStatus,
};
use orbit_engine::RuntimeHost;
use orbit_tools::ToolContext;
use orbit_types::task::HostOs;
use orbit_types::workflow::{JobRunState, JobRunTrigger, PipelineState};
use serde_json::{Value, json};
use tempfile::TempDir;

use super::dispatch_admission::isolated;

/// A workspace runtime admitting tasks as a host running `os` would.
fn runtime_on(os: HostOs) -> (TempDir, OrbitRuntime) {
    let root = TempDir::new().unwrap();
    let global = root.path().join("home/.orbit");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &workspace)
        .expect("build runtime")
        .with_host_os(Some(os));
    (root, runtime)
}

fn task(runtime: &OrbitRuntime, title: &str, tags: &[&str], status: TaskStatus) -> Task {
    let file = format!(
        "fixture-{}.txt",
        runtime.list_task_metadata().unwrap().len()
    );
    std::fs::write(runtime.paths().repo_root.join(&file), "fixture\n").unwrap();
    runtime
        .add_task(TaskAddParams {
            title: title.to_string(),
            description: format!("Fixture task: {title}"),
            acceptance_criteria: vec!["Fixture task is observable.".to_string()],
            plan: "Fixture plan.".to_string(),
            tags: tags.iter().map(ToString::to_string).collect(),
            complexity: TaskComplexity::Medium,
            context_files: vec![format!("file:{file}")],
            status: Some(status),
            ..Default::default()
        })
        .expect("seed task")
}

fn backlog(runtime: &OrbitRuntime) -> Value {
    runtime
        .run_deterministic(
            "list_backlog_tasks",
            &json!({}),
            &json!({}),
            ToolContext::default(),
        )
        .expect("list backlog tasks")
}

fn admitted(output: &Value) -> Vec<String> {
    output["task_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|id| id.as_str().unwrap().to_string())
        .collect()
}

fn excluded<'a>(output: &'a Value, task: &str) -> Option<&'a Value> {
    output["excluded"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"] == task)
}

/// A running `job` run, as its worker leaves it, with `input`.
fn running_run(runtime: &OrbitRuntime, job: &str, input: Value) -> String {
    let jobs = orbit_store::compose::workspace_job_run_store(
        runtime.sqlite_store().unwrap(),
        runtime.workspace_id().unwrap(),
    );
    let run = jobs
        .insert_job_run(job, 1, chrono::Utc::now(), Some(input), None)
        .expect("run");
    runtime
        .write_run_state(
            &run.run_id,
            &PipelineState::new(run.run_id.clone(), run.job_id, json!({})),
        )
        .expect("run state");
    jobs.mark_job_run_running(&run.run_id, chrono::Utc::now(), std::process::id())
        .expect("running");
    run.run_id
}

fn ship(runtime: &OrbitRuntime, task: &str) -> OrbitError {
    runtime
        .submit_ship_run(
            ShipMode::Local,
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

/// On a Linux host an `os:macos` task is withheld by discovery, the drain's
/// wave and readiness with the wait named, and an explicit ship refuses it
/// before anything is submitted. A task tagged for both OSes, and an untagged
/// one, are admitted exactly as before; a macOS host admits the macOS task.
#[test]
fn a_linux_host_withholds_and_refuses_a_macos_task_and_names_the_wait() {
    if !isolated(
        "host_os_routing::a_linux_host_withholds_and_refuses_a_macos_task_and_names_the_wait",
    ) {
        return;
    }
    let (_root, linux) = runtime_on(HostOs::Linux);
    // Matching ignores case; the stored tag is normalized.
    let mac = task(&linux, "mac", &["OS:MacOS"], TaskStatus::Backlog);
    let either = task(
        &linux,
        "either",
        &["os:linux", "os:macos"],
        TaskStatus::Backlog,
    );
    let anywhere = task(&linux, "anywhere", &[], TaskStatus::Backlog);
    assert_eq!(mac.tags, vec!["os:macos".to_string()]);
    let wait = "waits for a macos host (os:macos)";

    let discovered = backlog(&linux);
    assert_eq!(
        admitted(&discovered),
        vec![either.id.clone(), anywhere.id.clone()],
        "{discovered}"
    );
    let skipped = excluded(&discovered, &mac.id).expect("the macOS task is excluded");
    assert_eq!(skipped["reason"], "host_os_mismatch", "{discovered}");
    assert_eq!(skipped["detail"], wait, "{discovered}");

    let drain = running_run(&linux, "workspace_auto_pipeline", json!({}));
    let wave = linux
        .run_deterministic(
            "classify_workspace_auto_tasks",
            &json!({}),
            &json!({"run_id": drain, "max_active_leaf_runs": 4}),
            ToolContext::default(),
        )
        .expect("classify");
    assert_eq!(
        wave["loose_task_ids"],
        json!([either.id, anywhere.id]),
        "{wave}"
    );
    // `orbit run show` reads the wait off the drain's last pass.
    let pass = linux
        .read_run_state(&drain)
        .unwrap()
        .unwrap()
        .drain_last_pass
        .expect("last pass");
    let waiting = pass
        .excluded
        .iter()
        .find(|entry| entry.task_id == mac.id)
        .expect("the pass records the macOS task");
    assert_eq!(waiting.reason.as_deref(), Some("host_os_mismatch"));
    assert_eq!(waiting.detail.as_deref(), Some(wait));

    let readiness = linux.workspace_auto_readiness(&[], None, 50, &[]).unwrap();
    let entry = readiness["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["task_id"] == mac.id.as_str())
        .expect("readiness lists the macOS task");
    assert_eq!(entry["eligible"], false, "{readiness}");
    assert_eq!(entry["reason"], "host_os_mismatch", "{readiness}");
    assert_eq!(entry["detail"], wait, "{readiness}");
    let warning = linux
        .host_os_backlog_warning()
        .expect("a drain start warns");
    assert!(warning.contains(&format!("{} {wait}", mac.id)), "{warning}");

    let refused = ship(&linux, &mac.id);
    assert!(
        matches!(&refused, OrbitError::PolicyDenied(message) if message.contains(wait)),
        "{refused}"
    );
    let jobs = orbit_store::compose::workspace_job_run_store(
        linux.sqlite_store().unwrap(),
        linux.workspace_id().unwrap(),
    );
    assert!(
        jobs.list_job_runs("task_auto_pipeline").unwrap().is_empty(),
        "a refused ship starts nothing"
    );
    // The untagged task passes the OS check and stops only at this fixture's
    // missing job asset.
    assert!(
        !matches!(ship(&linux, &anywhere.id), OrbitError::PolicyDenied(_)),
        "an untagged task ships as before"
    );

    let mac_host = linux.clone().with_host_os(Some(HostOs::Macos));
    assert_eq!(
        admitted(&backlog(&mac_host)),
        vec![mac.id, either.id, anywhere.id]
    );
    assert!(mac_host.host_os_backlog_warning().is_none());
}

/// Retagging a backlog task applies at its next admission; retagging a task
/// already running leaves it, and the run carrying it, where they are.
#[test]
fn retagging_applies_at_the_next_admission_and_never_moves_running_work() {
    if !isolated(
        "host_os_routing::retagging_applies_at_the_next_admission_and_never_moves_running_work",
    ) {
        return;
    }
    let (_root, linux) = runtime_on(HostOs::Linux);
    let queued = task(&linux, "queued", &["ci-failure-sweep"], TaskStatus::Backlog);
    let running = task(&linux, "running", &[], TaskStatus::InProgress);
    let leaf = running_run(
        &linux,
        "task_auto_pipeline",
        json!({"task_ids": [running.id]}),
    );
    assert_eq!(admitted(&backlog(&linux)), vec![queued.id.clone()]);

    for id in [&queued.id, &running.id] {
        linux
            .run_tool(
                "orbit.task.update",
                json!({"id": id, "tags": ["ci-failure-sweep", "os:macos"], "model": "codex"}),
            )
            .expect("retag");
    }

    let next = backlog(&linux);
    assert!(admitted(&next).is_empty(), "{next}");
    assert_eq!(
        excluded(&next, &queued.id).expect("retagged task withheld")["reason"],
        "host_os_mismatch"
    );
    assert_eq!(
        linux.get_task(&running.id).unwrap().status,
        TaskStatus::InProgress
    );
    let jobs = orbit_store::compose::workspace_job_run_store(
        linux.sqlite_store().unwrap(),
        linux.workspace_id().unwrap(),
    );
    assert_eq!(
        jobs.get_job_run(&leaf).unwrap().unwrap().state,
        JobRunState::Running
    );
}

/// The `os:` namespace is reserved: `task.add` and `task.update` reject a
/// value outside it, naming the accepted ones, and write nothing.
#[test]
fn an_unsupported_os_tag_is_rejected_on_add_and_update() {
    if !isolated("host_os_routing::an_unsupported_os_tag_is_rejected_on_add_and_update") {
        return;
    }
    let (root, runtime) = runtime_on(HostOs::Linux);
    let workspace = root.path().join("repo").to_string_lossy().into_owned();
    let add = runtime
        .run_tool(
            "orbit.task.add",
            json!({
                "title": "Typo", "description": "Fixture.", "tags": ["os:mac"],
                "acceptance_criteria": ["Observable."], "complexity": "low", "model": "codex",
                "workspace": workspace,
            }),
        )
        .expect_err("os:mac is not an OS");
    assert!(
        matches!(&add, OrbitError::InvalidInput(message)
            if message.contains("`os:mac`") && message.contains("`os:macos`")),
        "{add}"
    );
    assert!(
        runtime.list_tasks().unwrap().is_empty(),
        "nothing was created"
    );

    let valid = task(&runtime, "valid", &["os:linux"], TaskStatus::Backlog);
    let update = runtime
        .run_tool(
            "orbit.task.update",
            json!({"id": valid.id, "tags": ["os:linux", "os:osx"], "model": "codex",
                "workspace": workspace}),
        )
        .expect_err("os:osx is not an OS");
    assert!(
        matches!(&update, OrbitError::InvalidInput(message) if message.contains("`os:osx`")),
        "{update}"
    );
    assert_eq!(
        runtime.get_task(&valid.id).unwrap().tags,
        vec!["os:linux".to_string()]
    );
}
