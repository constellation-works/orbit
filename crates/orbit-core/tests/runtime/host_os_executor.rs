//! An executor `host_os` narrows the task's effective OS requirement.
//!
//! The fixture composes the host OS, so a muse crew waits on Linux and is
//! admitted on macOS whether this process runs on either.

use std::collections::BTreeSet;

use orbit_core::application::task::TaskAddParams;
use orbit_core::bootstrap::init::{InitOptions, init_workspace_at_root};
use orbit_core::{
    CompletionPolicy, OrbitError, OrbitRuntime, ShipMode, Task, TaskComplexity, TaskStatus,
};
use orbit_engine::RuntimeHost;
use orbit_tools::ToolContext;
use orbit_types::task::HostOs;
use orbit_types::workflow::{ExecutorDef, ExecutorType, JobRunState, JobRunTrigger, PipelineState};
use serde_json::{Value, json};
use tempfile::TempDir;

/// Muse weighs far more than sol. A Linux draw still selects sol, because the
/// muse executor's `host_os` removes muse-wonder before the draw.
const POOL: &str = r#"
[workflow]
default_crew = "sol"
medium_complexity_crews = ["muse-wonder:1000", "sol:1"]

[crews.sol]
model = "gpt-6.1-sol"
provider = "codex"

[crews.muse-wonder]
model = "muse-spark-1.3"
provider = "muse"
"#;

/// No pool. The default crew's executor cannot run on Linux.
const DEFAULT_MUSE: &str = r#"
[workflow]
default_crew = "muse-wonder"

[crews.sol]
model = "gpt-6.1-sol"
provider = "codex"

[crews.muse-wonder]
model = "muse-spark-1.3"
provider = "muse"
"#;

const WAIT: &str = "waits for a macos host (crew muse-wonder)";

fn runtime_on(os: HostOs, config: &str) -> (TempDir, OrbitRuntime) {
    let root = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("home/.orbit");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(workspace.join("config.toml"), config).unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &workspace)
        .expect("build runtime")
        .with_host_resource_probe(super::dispatch_admission::PressureProbe::calm())
        .with_host_os(Some(os));
    // A fresh runtime has no executor store. Init seeds the shipped muse asset;
    // this fixture installs that same macOS-only declaration.
    declare_muse_macos(&runtime);
    (root, runtime)
}

/// The shipped muse executor: macOS only. Other providers stay undeclared.
pub(crate) fn declare_muse_macos(runtime: &OrbitRuntime) {
    runtime
        .upsert_executor_def(&ExecutorDef {
            name: "muse".to_string(),
            executor_type: ExecutorType::DirectAgent,
            command: Some("muse".to_string()),
            args: Vec::new(),
            stdout_format: None,
            model_pair_override: None,
            model_flag: None,
            timeout_seconds: None,
            auth_probe: None,
            env: Default::default(),
            sandbox: None,
            allow_fallback: false,
            host_os: Some(BTreeSet::from([HostOs::Macos])),
            created_at: None,
            updated_at: None,
        })
        .expect("seed muse host_os");
}

fn add(
    runtime: &OrbitRuntime,
    title: &str,
    crew: Option<&str>,
    tags: &[&str],
) -> Result<Task, OrbitError> {
    let file = format!("fixture-{title}.txt");
    std::fs::write(runtime.paths().repo_root.join(&file), "fixture\n").unwrap();
    runtime.add_task(TaskAddParams {
        title: title.to_string(),
        description: format!("Fixture task: {title}"),
        acceptance_criteria: vec!["Fixture task is observable.".to_string()],
        plan: "Fixture plan.".to_string(),
        tags: tags.iter().map(ToString::to_string).collect(),
        complexity: TaskComplexity::Medium,
        context_files: vec![format!("file:{file}")],
        status: Some(TaskStatus::Backlog),
        crew: crew.map(str::to_string),
        ..Default::default()
    })
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

fn running_run(runtime: &OrbitRuntime) -> String {
    let jobs = orbit_store::compose::workspace_job_run_store(
        runtime.sqlite_store().unwrap(),
        runtime.workspace_id().unwrap(),
    );
    let run = jobs
        .insert_job_run(
            "workspace_auto_pipeline",
            1,
            chrono::Utc::now(),
            Some(json!({})),
            None,
        )
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

/// Linux discovery, the local drain and an explicit ship leave a muse crew
/// waiting, and do not redraw it. A macOS host admits it. The pool draw skips
/// muse-wonder even though it carries almost all of the weight.
fn a_linux_host_waits_on_a_muse_crew_and_a_macos_host_admits_it() {
    let (_root, linux) = runtime_on(HostOs::Linux, POOL);
    let drawn = add(&linux, "drawn", None, &[]).expect("pool draw");
    assert_eq!(
        drawn.crew.as_deref(),
        Some("sol"),
        "muse-wonder is not drawn on Linux"
    );
    let muse = add(&linux, "muse", Some("muse-wonder"), &[]).expect("explicit crew");
    assert_eq!(muse.crew.as_deref(), Some("muse-wonder"));

    let discovered = backlog(&linux);
    assert_eq!(
        admitted(&discovered),
        vec![drawn.id.clone()],
        "{discovered}"
    );
    let skipped = excluded(&discovered, &muse.id).expect("muse is excluded");
    assert_eq!(skipped["reason"], "host_os_mismatch", "{discovered}");
    assert_eq!(skipped["detail"], WAIT, "{discovered}");
    assert_eq!(
        linux.get_task(&muse.id).unwrap().crew.as_deref(),
        Some("muse-wonder"),
        "an explicit muse crew is not redrawn"
    );

    let drain = running_run(&linux);
    let wave = linux
        .run_deterministic(
            "classify_workspace_auto_tasks",
            &json!({}),
            &json!({"run_id": drain, "max_active_leaf_runs": 4}),
            ToolContext::default(),
        )
        .expect("classify");
    assert_eq!(wave["loose_task_ids"], json!([drawn.id]), "{wave}");
    let pass = linux
        .read_run_state(&drain)
        .unwrap()
        .unwrap()
        .pull_drain
        .drain_last_pass
        .expect("last pass");
    let waiting = pass
        .excluded
        .iter()
        .find(|entry| entry.task_id == muse.id)
        .expect("the pass records the muse task");
    assert_eq!(waiting.reason.as_deref(), Some("host_os_mismatch"));
    assert_eq!(waiting.detail.as_deref(), Some(WAIT));

    let readiness = linux.workspace_auto_readiness(&[], None, 50, &[]).unwrap();
    let entry = readiness["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["task_id"] == muse.id.as_str())
        .expect("readiness lists the muse task");
    assert_eq!(entry["reason"], "host_os_mismatch", "{readiness}");
    assert_eq!(entry["detail"], WAIT, "{readiness}");
    let warning = linux.host_os_backlog_warning().expect("drain start warns");
    assert!(
        warning.contains(&format!("{} {WAIT}", muse.id)),
        "{warning}"
    );

    let refused = ship(&linux, &muse.id);
    assert!(
        matches!(&refused, OrbitError::PolicyDenied(message) if message.contains(WAIT)),
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

    let mac = linux.clone().with_host_os(Some(HostOs::Macos));
    assert_eq!(
        admitted(&backlog(&mac)),
        vec![drawn.id.clone(), muse.id.clone()]
    );
    let mac_drain = running_run(&mac);
    let mac_wave = mac
        .run_deterministic(
            "classify_workspace_auto_tasks",
            &json!({}),
            &json!({"run_id": mac_drain, "max_active_leaf_runs": 4}),
            ToolContext::default(),
        )
        .expect("classify on macOS");
    assert_eq!(
        mac_wave["loose_task_ids"],
        json!([drawn.id, muse.id]),
        "{mac_wave}"
    );
    assert!(mac.host_os_backlog_warning().is_none());
    assert_eq!(
        mac.get_task(&muse.id).unwrap().crew.as_deref(),
        Some("muse-wonder")
    );
}

/// `os:linux` assigned to a macOS-only crew is refused before it is stored.
fn an_empty_os_intersection_is_refused_on_add_and_update() {
    let (_root, linux) = runtime_on(HostOs::Linux, POOL);
    let reason = "crew `muse-wonder` (executor `muse`, host_os: macos) does not intersect \
         the task's tags `os:linux`; no host can run it";
    let refused = add(&linux, "conflict", Some("muse-wonder"), &["os:linux"])
        .expect_err("no host can run this assignment");
    assert!(
        matches!(&refused, OrbitError::InvalidInput(message) if message.contains(reason)),
        "{refused}"
    );
    assert!(linux.list_tasks().unwrap().is_empty(), "nothing was stored");

    let stored = add(&linux, "stored", Some("sol"), &["os:linux"]).expect("sol runs on linux");
    let update = linux
        .run_tool(
            "orbit.task.update",
            json!({
                "id": stored.id,
                "crew": "muse-wonder",
                "model": "codex",
            }),
        )
        .expect_err("retargeting onto muse does not intersect os:linux");
    assert!(
        matches!(&update, OrbitError::InvalidInput(message) if message.contains(reason)),
        "{update}"
    );
    let kept = linux.get_task(&stored.id).unwrap();
    assert_eq!(kept.crew.as_deref(), Some("sol"));
    assert_eq!(kept.tags, vec!["os:linux".to_string()]);
}

/// Replay an empty delivery job in the foreground: real automatic crew
/// admission runs, but no worker or provider is launched.
fn select_task_crew(runtime: &OrbitRuntime, task: &str) -> Result<Value, OrbitError> {
    let jobs_dir = runtime.paths().global_dir.join("resources/jobs");
    std::fs::create_dir_all(&jobs_dir).unwrap();
    std::fs::write(
        jobs_dir.join("task_auto_pipeline.yaml"),
        "schemaVersion: 2\nkind: Job\nmetadata:\n  name: task_auto_pipeline\nspec:\n  state: enabled\n  steps: []\n",
    ).unwrap();
    let jobs = orbit_store::compose::workspace_job_run_store(
        runtime.sqlite_store().unwrap(),
        runtime.workspace_id().unwrap(),
    );
    let source = jobs
        .insert_job_run(
            "task_auto_pipeline",
            1,
            chrono::Utc::now(),
            Some(json!({"task_ids": [task]})),
            None,
        )
        .unwrap();
    jobs.mark_job_run_running(&source.run_id, chrono::Utc::now(), std::process::id())
        .unwrap();
    jobs.finalize_job_run(
        &source.run_id,
        JobRunState::Failed,
        chrono::Utc::now(),
        Some(1),
    )
    .unwrap();
    let before = jobs.list_job_runs("task_auto_pipeline").unwrap().len();
    let replay = runtime.replay_job_run(&source.run_id);
    if replay.is_err() {
        assert_eq!(
            jobs.list_job_runs("task_auto_pipeline").unwrap().len(),
            before,
            "refusal precedes delivery run insertion"
        );
    }
    let replay = replay?;
    assert!(replay.success, "{replay:?}");
    Ok(jobs
        .get_job_run(&replay.run_id)
        .unwrap()
        .unwrap()
        .input
        .unwrap())
}

fn install_discovery_worker(runtime: &OrbitRuntime) {
    let worker = "host_os_executor::discovery_gate_worker";
    orbit_common::test_env::assert_child_test_exists(worker);
    orbit_core::test_support::install_substitute_pipeline_worker([
        "env".to_string(),
        format!(
            "ORBIT_TEST_OS_GLOBAL={}",
            runtime.paths().global_dir.display()
        ),
        format!(
            "ORBIT_TEST_OS_WORKSPACE={}",
            runtime.paths().orbit_dir.display()
        ),
        "ORBIT_TEST_OS_RUN={run_id}".to_string(),
        std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .into_owned(),
        "--exact".into(),
        worker.into(),
        "--ignored".into(),
        "--nocapture".into(),
    ]);
}

/// A substitute gate worker executes only the deterministic fixture job. The
/// real coordinator still performs admission, discovery and child dispatch.
#[test]
#[ignore = "entry point for the host OS pipeline fixture worker"]
fn discovery_gate_worker() {
    let global = std::env::var_os("ORBIT_TEST_OS_GLOBAL").unwrap();
    let workspace = std::env::var_os("ORBIT_TEST_OS_WORKSPACE").unwrap();
    let run = std::env::var("ORBIT_TEST_OS_RUN").unwrap();
    let runtime = OrbitRuntime::from_roots(
        std::path::Path::new(&global),
        std::path::Path::new(&workspace),
    )
    .unwrap()
    .with_host_resource_probe(super::dispatch_admission::PressureProbe::calm())
    .with_host_os(Some(HostOs::Linux));
    install_discovery_worker(&runtime);
    runtime.execute_pipeline_run_worker(&run).unwrap();
}

/// Exercise the shipped list/validate/fan-out path, rather than an empty job
/// that proves only initial admission. No provider or delivery is launched.
fn a_supported_explicit_run_crew_survives_discovery_and_replay() {
    let (_root, linux) = runtime_on(HostOs::Linux, DEFAULT_MUSE);
    let task = add(&linux, "run-override", None, &[]).unwrap();
    assert_eq!(task.crew, None);
    let resources = linux.paths().global_dir.join("resources");
    std::fs::create_dir_all(resources.join("jobs")).unwrap();
    std::fs::create_dir_all(resources.join("activities")).unwrap();
    for name in [
        "list_backlog_tasks",
        "validate_bundles",
        "invoke_and_wait",
        "pipeline_success_guard",
    ] {
        std::fs::copy(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(format!("assets/activities/{name}.yaml")),
            resources.join(format!("activities/{name}.yaml")),
        )
        .unwrap();
    }
    let job_path = resources.join("jobs/task_auto_pipeline.yaml");
    let shipped = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("assets/jobs/task_auto_pipeline.yaml"),
    )
    .unwrap();
    let mut job: Value = serde_yaml::from_str(&shipped).unwrap();
    // Bound a broken fixture worker without changing discovery or dispatch.
    job["spec"]["steps"][2]["fan_out"]["worker"]["default_input"]["timeout_seconds"] = json!(30);
    std::fs::write(&job_path, serde_yaml::to_string(&job).unwrap()).unwrap();
    std::fs::write(resources.join("jobs/task_gate_pipeline.yaml"),
        "schemaVersion: 2\nkind: Job\nmetadata:\n  name: task_gate_pipeline\nspec:\n  state: enabled\n  steps: []\n",
    ).unwrap();
    install_discovery_worker(&linux);
    let jobs = orbit_store::compose::workspace_job_run_store(
        linux.sqlite_store().unwrap(),
        linux.workspace_id().unwrap(),
    );
    // Ordinary submission captures the explicit selection before discovery.
    // Replay then keeps that choice and exercises the same shipped steps.
    let submitted = linux
        .submit_pipeline_run(
            "task_auto_pipeline",
            json!({"task_ids": [task.id], "crew": "sol", "mode": "local"}),
            None,
            Some("test"),
        )
        .unwrap();
    let waited = linux
        .run_tool(
            "orbit.pipeline.wait",
            json!({"run_ids": [submitted.run_id], "timeout_seconds": 60}),
        )
        .unwrap();
    assert_eq!(
        jobs.get_job_run(&submitted.run_id).unwrap().unwrap().state,
        JobRunState::Success,
        "{waited}"
    );
    let original_children = jobs.list_job_runs("task_gate_pipeline").unwrap();
    assert_eq!(
        original_children.len(),
        1,
        "the original explicit submission dispatches a gate"
    );
    assert_eq!(original_children[0].input.as_ref().unwrap()["crew"], "sol");
    let replay = linux.replay_job_run(&submitted.run_id).unwrap();
    assert!(replay.success, "{replay:?}");
    assert_eq!(
        admitted(&replay.pipeline["list_backlog"]),
        vec![task.id.clone()],
        "{replay:?}"
    );
    assert_eq!(replay.pipeline["validate_bundles"]["bundle_count"], 1);
    let input = jobs
        .get_job_run(&replay.run_id)
        .unwrap()
        .unwrap()
        .input
        .unwrap();
    assert_eq!(input["crew_selection"]["source"], "explicit");
    assert_eq!(input["crew_selection"]["crew"], "sol");
    assert_eq!(jobs.list_job_runs("task_gate_pipeline").unwrap().len(), 2);
    let child_id = replay.pipeline["gate_results"][0]["run_id"]
        .as_str()
        .unwrap();
    let child = jobs.get_job_run(child_id).unwrap().unwrap();
    assert_eq!(child.state, JobRunState::Success);
    let child_input = child.input.as_ref().unwrap();
    assert_eq!(child_input["crew"], "sol");
    assert_eq!(child_input["crew_selection"], input["crew_selection"]);

    // A frozen choice is scoped to its task and cannot erase task pins or
    // OS tags, even when a caller names additional tasks at discovery.
    let pinned = add(&linux, "pinned", Some("muse-wonder"), &[]).unwrap();
    let tagged = add(&linux, "tagged", None, &["os:macos"]).unwrap();
    let other = add(&linux, "other", None, &[]).unwrap();
    let listed = linux.run_deterministic("list_backlog_tasks", &json!({}),
        &json!({"run_id": replay.run_id, "task_ids": [task.id, pinned.id, tagged.id, other.id]}),
        ToolContext::default()).unwrap();
    assert_eq!(admitted(&listed), vec![task.id.clone()]);
    for id in [&pinned.id, &tagged.id, &other.id] {
        assert_eq!(
            excluded(&listed, id).unwrap()["reason"],
            "host_os_mismatch",
            "{listed}"
        );
    }
    assert_eq!(
        linux.get_task(&task.id).unwrap().crew,
        None,
        "run override does not pin the task"
    );
    // Even a matching frozen selection does not release a task's stored pin
    // or its tags if either changes after admission.
    for update in [
        json!({"crew": "muse-wonder"}),
        json!({"crew": "sol", "tags": ["os:macos"]}),
    ] {
        let mut update = update;
        update["id"] = json!(task.id);
        update["model"] = json!("codex");
        linux.run_tool("orbit.task.update", update).unwrap();
        let listed = linux
            .run_deterministic(
                "list_backlog_tasks",
                &json!({}),
                &json!({"run_id": replay.run_id, "task_ids": [task.id]}),
                ToolContext::default(),
            )
            .unwrap();
        assert!(admitted(&listed).is_empty(), "{listed}");
        assert_eq!(
            excluded(&listed, &task.id).unwrap()["reason"],
            "host_os_mismatch"
        );
    }
}

/// Unsupported defaults stay unpinned but cannot be recovered by a later
/// automatic draw, including when host filtering empties a configured pool.
fn an_excluded_default_crew_is_not_pinned() {
    for pool in [
        "",
        "medium_complexity_crews = [\"muse-wonder\"]",
        "medium_complexity_crews = [\"muse-wonder\"]\n[workflow.purpose_crews]\nbaseline_red_repair = [\"muse-wonder\"]",
    ] {
        let config = DEFAULT_MUSE.replace(
            "default_crew = \"muse-wonder\"",
            &format!("default_crew = \"muse-wonder\"\n{pool}"),
        );
        let (_root, linux) = runtime_on(HostOs::Linux, &config);
        let tags = if pool.contains("purpose_crews") {
            vec!["baseline-red-repair"]
        } else {
            vec![]
        };
        let unpinned = add(&linux, "unpinned", None, &tags).expect("creation stays possible");
        assert_eq!(unpinned.crew, None, "the excluded default is not pinned");
        let refused = select_task_crew(&linux, &unpinned.id)
            .expect_err("automatic draw must refuse the unsupported default");
        assert!(
            matches!(&refused, OrbitError::PolicyDenied(message) if message.contains(WAIT)),
            "{refused}"
        );
        let discovered = backlog(&linux);
        assert!(admitted(&discovered).is_empty(), "{discovered}");
        let withheld = excluded(&discovered, &unpinned.id).unwrap();
        assert_eq!(withheld["reason"], "host_os_mismatch", "{discovered}");
        assert!(
            withheld["detail"].as_str().unwrap().contains(WAIT),
            "{discovered}"
        );

        if pool.is_empty() {
            let explicit = linux
                .run_deterministic(
                    "list_backlog_tasks",
                    &json!({}),
                    &json!({"task_ids": [unpinned.id]}),
                    ToolContext::default(),
                )
                .unwrap();
            assert!(admitted(&explicit).is_empty(), "{explicit}");
            assert_eq!(
                excluded(&explicit, &unpinned.id).unwrap()["reason"],
                "host_os_mismatch"
            );
            let warning = linux
                .host_os_backlog_warning()
                .expect("the drain explains its wait");
            assert!(warning.contains(WAIT), "{warning}");
            let readiness = linux.workspace_auto_readiness(&[], None, 50, &[]).unwrap();
            let entry = readiness["tasks"]
                .as_array()
                .unwrap()
                .iter()
                .find(|entry| entry["task_id"] == unpinned.id)
                .unwrap();
            assert_eq!(entry["reason"], "host_os_mismatch", "{readiness}");
            let run = running_run(&linux);
            let wave = linux
                .run_deterministic(
                    "classify_workspace_auto_tasks",
                    &json!({}),
                    &json!({"run_id": run, "max_active_leaf_runs": 4}),
                    ToolContext::default(),
                )
                .unwrap();
            assert_eq!(wave["loose_task_ids"], json!([]), "{wave}");
            let refused = ship(&linux, &unpinned.id);
            assert!(
                matches!(&refused, OrbitError::PolicyDenied(message) if message.contains(WAIT)),
                "{refused}"
            );
        }
        let mac = linux.clone().with_host_os(Some(HostOs::Macos));
        assert!(admitted(&backlog(&mac)).contains(&unpinned.id));
        let selected = select_task_crew(&mac, &unpinned.id)
            .expect("macOS draws the default or supported pool");
        assert_eq!(selected["crew_selection"]["crew"], "muse-wonder");
        let pinned = add(&mac, "pinned", None, &tags).expect("macOS pins the default or pool");
        assert_eq!(pinned.crew.as_deref(), Some("muse-wonder"));
        if pool.is_empty() {
            // Also exercise the default-provenance fallback, rather than only
            // the unassigned-task fallback above.
            let refused = select_task_crew(&linux, &pinned.id)
                .expect_err("a default assignment is not redrawn onto an unsupported default");
            assert!(
                matches!(&refused, OrbitError::PolicyDenied(message) if message.contains(WAIT)),
                "{refused}"
            );
        }
    }
    let config = POOL.replace("default_crew = \"sol\"", "default_crew = \"muse-wonder\"");
    let (_root, linux) = runtime_on(HostOs::Linux, &config);
    let supported = add(&linux, "supported", None, &[]).unwrap();
    assert_eq!(supported.crew.as_deref(), Some("sol"));
    assert!(admitted(&backlog(&linux)).contains(&supported.id));
    assert_eq!(
        select_task_crew(&linux, &supported.id).unwrap()["crew_selection"]["crew"],
        "sol",
        "a supported pool remains usable beneath an unsupported default"
    );
}

/// Both ordinary and native-inspection activity defaults obey executor host_os.
/// Undeclared executors and explicit task pins keep their existing behavior.
fn activity_defaults_and_unrestricted_crews_remain_consistent() {
    let config = DEFAULT_MUSE
        .replace("provider = \"muse\"", "provider = \"codex\"")
        .replace(
            "default_crew = \"muse-wonder\"",
            "default_crew = \"muse-wonder\"\nmedium_complexity_crews = [\"muse-wonder\"]\n[workflow.purpose_crews]\nfailure_recovery = [\"muse-wonder\"]",
        );
    let (_root, linux) = runtime_on(HostOs::Linux, &config);
    let mut executor = linux
        .list_executor_defs()
        .unwrap()
        .into_iter()
        .find(|def| def.name == "muse")
        .unwrap();
    executor.name = "codex".into();
    linux.upsert_executor_def(&executor).unwrap();
    for (key, inspection) in [
        ("workflow.medium_complexity_crews", false),
        ("workflow.medium_complexity_crews", true),
        ("workflow.purpose_crews.failure_recovery", false),
        ("workflow.purpose_crews.failure_recovery", true),
    ] {
        let run = running_run(&linux);
        let mut input = json!({"run_id": run, "crew_config_key": key});
        if inspection {
            input["inspection_revision"] = json!("fixture-revision");
            input["partition_index"] = json!(0);
        }
        let refused = linux
            .agent_crew_config_for_input(&input)
            .expect_err("unsupported activity default is refused");
        assert!(refused.to_string().contains(WAIT), "{refused}");
        assert!(
            linux
                .read_run_state(&run)
                .unwrap()
                .unwrap()
                .activity_crew_draws
                .is_empty()
        );
        let mac = linux.clone().with_host_os(Some(HostOs::Macos));
        let selected = mac.agent_crew_config_for_input(&input).unwrap().unwrap();
        assert_eq!(selected.model.as_deref(), Some("muse-spark-1.3"));
    }
    executor.host_os = None;
    linux.upsert_executor_def(&executor).unwrap();
    let unbounded = add(&linux, "unbounded", None, &[]).unwrap();
    assert_eq!(unbounded.crew.as_deref(), Some("muse-wonder"));
    assert!(admitted(&backlog(&linux)).contains(&unbounded.id));
    assert_eq!(
        select_task_crew(&linux, &unbounded.id).unwrap()["crew_selection"]["crew"],
        "muse-wonder"
    );
    let explicit = add(&linux, "explicit", Some("sol"), &[]).unwrap();
    assert_eq!(
        select_task_crew(&linux, &explicit.id).unwrap()["crew_selection"]["crew"],
        "sol"
    );
}

/// Public init loads the shipped declarations and upgrades an installed default
/// only when its host constraint is absent. Operator choices survive refresh.
fn init_seeds_host_os_and_preserves_operator_constraints() {
    let root = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("global");
    let seed = || {
        init_workspace_at_root(
            &global,
            InitOptions {
                global_only: true,
                refresh_defaults: true,
                ..Default::default()
            },
        )
        .expect("seed defaults through public init");
    };
    seed();
    let store = orbit_store::compose::global_executor_def_store(global.join("resources/executors"));
    let defs = store.list_executor_defs().expect("load seeded catalog");
    assert!(defs.len() > 1, "the shipped catalog is more than muse");
    let mut muse = store
        .get_executor_def("muse")
        .unwrap()
        .expect("muse seeded");
    assert_eq!(muse.host_os, Some(BTreeSet::from([HostOs::Macos])));
    for def in defs.into_iter().filter(|def| def.name != "muse") {
        assert!(
            def.host_os.is_none(),
            "{} declared host_os {:?}",
            def.name,
            def.host_os
        );
    }

    muse.host_os = None;
    store
        .upsert_executor_def(&muse)
        .expect("installed default without host_os");
    seed();
    let copied = store
        .get_executor_def("muse")
        .unwrap()
        .expect("muse present");
    assert_eq!(copied.host_os, Some(BTreeSet::from([HostOs::Macos])));

    let mut customized = copied;
    customized.host_os = Some(BTreeSet::from([HostOs::Linux, HostOs::Macos]));
    store
        .upsert_executor_def(&customized)
        .expect("operator host_os");
    seed();
    assert_eq!(
        store
            .get_executor_def("muse")
            .unwrap()
            .expect("muse present")
            .host_os,
        customized.host_os,
        "an operator-set host_os is not overwritten"
    );

    customized.host_os = None;
    store
        .upsert_executor_def(&customized)
        .expect("clear host_os");
    seed();
    assert_eq!(
        store
            .get_executor_def("muse")
            .unwrap()
            .expect("muse present")
            .host_os,
        Some(BTreeSet::from([HostOs::Macos])),
        "clearing host_os restores the shipped declaration"
    );
}

crate::scenario_table!(
    a_linux_host_waits_on_a_muse_crew_and_a_macos_host_admits_it_scenarios,
    a_linux_host_waits_on_a_muse_crew_and_a_macos_host_admits_it,
    an_empty_os_intersection_is_refused_on_add_and_update,
    an_excluded_default_crew_is_not_pinned,
    a_supported_explicit_run_crew_survives_discovery_and_replay,
    init_seeds_host_os_and_preserves_operator_constraints,
    activity_defaults_and_unrestricted_crews_remain_consistent
);
