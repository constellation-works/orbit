//! The `ui` tag routes a task to a host with a verified browser kit.
//!
//! Local admission — the drain's wave, ship discovery, readiness and an
//! explicit ship — reads the host's kit on every pass. The fixture composes
//! the host OS and the home whose kit admission reads, so each test behaves
//! the same whether or not the machine running it has a kit.

use std::path::Path;

use orbit_core::application::task::TaskAddParams;
use orbit_core::bootstrap::browser_kit::{KitLayout, PLAYWRIGHT_VERSION};
use orbit_core::{
    CompletionPolicy, OrbitError, OrbitRuntime, ShipMode, Task, TaskComplexity, TaskStatus,
};
use orbit_engine::RuntimeHost;
use orbit_tools::ToolContext;
use orbit_types::task::{HostOs, browser_kit_wait};
use orbit_types::workflow::{JobRunTrigger, PipelineState};
use serde_json::{Value, json};
use tempfile::TempDir;

const REVISION: &str = "1248";

/// Lay out a kit under `home` as `orbit browser-kit install` leaves it, with
/// the smoke-test record `verify` writes only when `verified`.
pub(crate) fn write_kit(home: &Path, verified: bool) {
    let layout = KitLayout::for_home(home);
    let root = layout.root();
    let package = root.join("kit/node_modules/playwright");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(
        package.join("package.json"),
        json!({"name": "playwright", "version": PLAYWRIGHT_VERSION}).to_string(),
    )
    .unwrap();
    std::fs::write(package.join("index.mjs"), "export {};\n").unwrap();
    let core = root.join("kit/node_modules/playwright-core");
    std::fs::create_dir_all(&core).unwrap();
    std::fs::write(
        core.join("browsers.json"),
        json!({"browsers": [{"name": "chromium-headless-shell", "revision": REVISION}]})
            .to_string(),
    )
    .unwrap();
    let shell = root.join(format!("browsers/chromium_headless_shell-{REVISION}"));
    std::fs::create_dir_all(&shell).unwrap();
    std::fs::write(shell.join("INSTALLATION_COMPLETE"), "").unwrap();
    std::fs::write(root.join("env.sh"), "# fixture\n").unwrap();
    if verified {
        std::fs::write(
            layout.verification_file(),
            json!({
                "schema_version": 1,
                "playwright_version": PLAYWRIGHT_VERSION,
                "chromium_revision": REVISION,
                "browser_version": "156.0.8078.4",
                "verified_at": "2026-10-18T00:00:00Z",
            })
            .to_string(),
        )
        .unwrap();
    }
}

/// A workspace runtime admitting tasks as a host running `os` would, reading
/// its browser kit from `<root>/kit-home`, which starts empty.
fn runtime_on(os: HostOs, config: &str) -> (TempDir, OrbitRuntime) {
    let root = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("home/.orbit");
    let workspace = root.path().join("repo/.orbit");
    let kit_home = root.path().join("kit-home");
    for dir in [&global, &workspace, &kit_home] {
        std::fs::create_dir_all(dir).unwrap();
    }
    std::fs::write(workspace.join("config.toml"), config).unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &workspace)
        .expect("build runtime")
        .with_host_resource_probe(super::dispatch_admission::PressureProbe::calm())
        .with_host_os(Some(os))
        .with_browser_kit_home(&kit_home);
    (root, runtime)
}

fn task(runtime: &OrbitRuntime, title: &str, crew: Option<&str>, tags: &[&str]) -> Task {
    let file = format!("fixture-{title}.txt");
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
            status: Some(TaskStatus::Backlog),
            crew: crew.map(str::to_string),
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

/// The reason and detail discovery withholds `task` with.
fn exclusion(output: &Value, task: &str) -> Option<(String, String)> {
    output["excluded"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"] == task)
        .map(|entry| {
            (
                entry["reason"].as_str().unwrap_or_default().to_string(),
                entry["detail"].as_str().unwrap_or_default().to_string(),
            )
        })
}

fn running_drain(runtime: &OrbitRuntime) -> String {
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

/// A host whose kit is absent, or present but never verified, withholds a
/// `ui` task from discovery, the drain's wave and readiness with the wait
/// named, refuses an explicit ship of it, and lists it for `orbit doctor`.
/// Topical tags route nothing. Verifying the kit mid-window admits the task
/// at the next pass, with no restart.
fn a_host_without_a_verified_kit_withholds_ui_tasks_until_its_kit_is_verified() {
    let (root, linux) = runtime_on(HostOs::Linux, "");
    // Matching ignores case.
    let ui = task(&linux, "ui", None, &["UI"]);
    let topical = task(&linux, "topical", None, &["dashboard", "ux"]);
    let wait = browser_kit_wait();
    assert!(wait.contains("`orbit browser-kit install`"), "{wait}");

    let discovered = backlog(&linux);
    assert_eq!(
        admitted(&discovered),
        vec![topical.id.clone()],
        "{discovered}"
    );
    assert_eq!(
        exclusion(&discovered, &ui.id),
        Some(("browser_kit_missing".to_string(), wait.clone())),
        "{discovered}"
    );

    let drain = running_drain(&linux);
    let wave = linux
        .run_deterministic(
            "classify_workspace_auto_tasks",
            &json!({}),
            &json!({"run_id": drain, "max_active_leaf_runs": 4}),
            ToolContext::default(),
        )
        .expect("classify");
    assert_eq!(wave["loose_task_ids"], json!([topical.id]), "{wave}");
    // `orbit run show` and the dashboard Drain card read the drain's last pass.
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
        .find(|entry| entry.task_id == ui.id)
        .expect("the pass records the ui task");
    assert_eq!(waiting.reason.as_deref(), Some("browser_kit_missing"));
    assert_eq!(waiting.detail.as_deref(), Some(wait.as_str()));

    let readiness = linux.workspace_auto_readiness(&[], None, 50, &[]).unwrap();
    let entry = readiness["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["task_id"] == ui.id.as_str())
        .expect("readiness lists the ui task");
    assert_eq!(entry["eligible"], false, "{readiness}");
    assert_eq!(entry["reason"], "browser_kit_missing", "{readiness}");
    assert_eq!(entry["detail"], wait.as_str(), "{readiness}");

    let refused = ship(&linux, &ui.id);
    assert!(
        matches!(&refused, OrbitError::PolicyDenied(message) if message.contains(&wait)),
        "{refused}"
    );
    assert!(
        !matches!(ship(&linux, &topical.id), OrbitError::PolicyDenied(_)),
        "a topical tag ships as before"
    );
    assert_eq!(
        linux.browser_kit_backlog_waits().unwrap(),
        vec![ui.id.clone()]
    );

    // Installed but never verified is not a kit admission trusts.
    let kit_home = root.path().join("kit-home");
    write_kit(&kit_home, false);
    assert_eq!(
        exclusion(&backlog(&linux), &ui.id).map(|(reason, _)| reason),
        Some("browser_kit_missing".to_string())
    );

    write_kit(&kit_home, true);
    let next = backlog(&linux);
    assert_eq!(admitted(&next), vec![ui.id.clone(), topical.id], "{next}");
    assert!(linux.browser_kit_backlog_waits().unwrap().is_empty());
    assert!(
        !matches!(ship(&linux, &ui.id), OrbitError::PolicyDenied(_)),
        "a verified kit lets the ui task ship"
    );
}

/// Muse weighs nothing here: it is the crew a task names, and its executor
/// runs only on macOS.
const MUSE: &str = r#"
[workflow]
default_crew = "sol"

[crews.sol]
model = "gpt-6.1-sol"
provider = "codex"

[crews.muse-wonder]
model = "muse-spark-1.3"
provider = "muse"
"#;

/// A host must satisfy every requirement: the `os:` tags, the crew's
/// executor `host_os` and the kit. The OS wait is named first, since a kit
/// would not let this host run the task, and `orbit doctor` counts only the
/// `ui` tasks the kit alone holds back.
fn ui_combines_with_os_tags_and_the_crew_os_requirement() {
    let (root, linux) = runtime_on(HostOs::Linux, MUSE);
    super::host_os_executor::declare_muse_macos(&linux);
    let mac_ui = task(&linux, "mac-ui", None, &["os:macos", "ui"]);
    let linux_ui = task(&linux, "linux-ui", None, &["os:linux", "ui"]);
    let muse_ui = task(&linux, "muse-ui", Some("muse-wonder"), &["ui"]);
    let reason = |runtime: &OrbitRuntime, task: &Task| {
        exclusion(&backlog(runtime), &task.id).map(|(reason, detail)| {
            assert!(!detail.is_empty(), "{reason} names its wait");
            reason
        })
    };

    // No kit on Linux: the kit holds back only the task Linux could run.
    assert_eq!(reason(&linux, &mac_ui).as_deref(), Some("host_os_mismatch"));
    assert_eq!(
        reason(&linux, &muse_ui).as_deref(),
        Some("host_os_mismatch")
    );
    assert_eq!(
        reason(&linux, &linux_ui).as_deref(),
        Some("browser_kit_missing")
    );
    assert_eq!(
        linux.browser_kit_backlog_waits().unwrap(),
        vec![linux_ui.id.clone()]
    );

    // No kit on macOS: the OS is right for the macOS and muse tasks, so the
    // kit is what they wait for.
    let mac = linux.clone().with_host_os(Some(HostOs::Macos));
    for task in [&mac_ui, &muse_ui] {
        assert_eq!(reason(&mac, task).as_deref(), Some("browser_kit_missing"));
    }
    assert_eq!(reason(&mac, &linux_ui).as_deref(), Some("host_os_mismatch"));

    // A verified kit satisfies only the kit.
    write_kit(&root.path().join("kit-home"), true);
    assert_eq!(admitted(&backlog(&linux)), vec![linux_ui.id.clone()]);
    assert_eq!(reason(&linux, &mac_ui).as_deref(), Some("host_os_mismatch"));
    assert_eq!(
        reason(&linux, &muse_ui).as_deref(),
        Some("host_os_mismatch")
    );
    let mut on_mac = admitted(&backlog(&mac));
    on_mac.sort();
    let mut expected = vec![mac_ui.id, muse_ui.id];
    expected.sort();
    assert_eq!(on_mac, expected);
}

// Related boundary scenarios share the isolated table fixture.
crate::scenario_table!(
    a_host_without_a_verified_kit_withholds_ui_tasks_until_its_kit_is_verified_scenarios,
    a_host_without_a_verified_kit_withholds_ui_tasks_until_its_kit_is_verified,
    ui_combines_with_os_tags_and_the_crew_os_requirement
);
