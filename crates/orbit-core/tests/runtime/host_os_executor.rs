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
use orbit_types::workflow::{ExecutorDef, ExecutorType, JobRunTrigger, PipelineState};
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
fn declare_muse_macos(runtime: &OrbitRuntime) {
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

/// A default crew this host cannot run is left unset, and a macOS host pins it.
fn an_excluded_default_crew_is_not_pinned() {
    let (_root, linux) = runtime_on(HostOs::Linux, DEFAULT_MUSE);
    let unpinned = add(&linux, "unpinned", None, &[]).expect("creation stays possible");
    assert_eq!(unpinned.crew, None, "the excluded default is not pinned");
    assert_eq!(
        admitted(&backlog(&linux)),
        vec![unpinned.id.clone()],
        "an unassigned task is not withheld for a default it does not carry"
    );

    let mac = linux.clone().with_host_os(Some(HostOs::Macos));
    let pinned = add(&mac, "pinned", None, &[]).expect("macOS pins the default");
    assert_eq!(pinned.crew.as_deref(), Some("muse-wonder"));
    assert!(admitted(&backlog(&mac)).contains(&pinned.id));
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
    init_seeds_host_os_and_preserves_operator_constraints
);
