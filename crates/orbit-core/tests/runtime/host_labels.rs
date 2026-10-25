//! A task's `requires_labels` route it to a host carrying every label.
//!
//! A host carries its `machine.labels` plus its OS. Local admission withholds
//! a task this host lacks a label for and names the wait; a write no known
//! host could serve is refused. The fixture composes the host OS and labels,
//! so each test behaves the same on every machine CI runs it on.

use orbit_core::application::task::TaskAddParams;
use orbit_core::{
    CompletionPolicy, OrbitError, OrbitRuntime, ShipMode, Task, TaskComplexity, TaskStatus,
};
use orbit_engine::RuntimeHost;
use orbit_tools::ToolContext;
use orbit_types::task::HostOs;
use orbit_types::workflow::JobRunTrigger;
use serde_json::{Value, json};
use tempfile::TempDir;

/// A workspace runtime admitting tasks as a host running `os` with
/// `machine.labels` set to `labels` (already normalized) would.
fn runtime_on(os: HostOs, labels: &[&str]) -> (TempDir, OrbitRuntime) {
    let root = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("home/.orbit");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &workspace)
        .expect("build runtime")
        .with_host_resource_probe(super::dispatch_admission::PressureProbe::calm())
        .with_host_os(Some(os))
        .with_machine_labels(labels.iter().map(ToString::to_string).collect());
    (root, runtime)
}

fn task(runtime: &OrbitRuntime, title: &str, tags: &[&str], labels: &[&str]) -> Task {
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
            requires_labels: labels.iter().map(ToString::to_string).collect(),
            complexity: TaskComplexity::Medium,
            context_files: vec![format!("file:{file}")],
            status: Some(TaskStatus::Backlog),
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

fn show(runtime: &OrbitRuntime, task: &str) -> Value {
    runtime
        .run_tool("orbit.task.show", json!({"id": task}))
        .expect("show task")
}

/// A host carrying `gpu` admits a task requiring it, normalized from `GPU`,
/// and its show names no wait. The same workspace read by a host without the
/// label withholds the task from discovery and readiness with the wait named,
/// refuses an explicit ship, and shows and lists the placement wait. A task
/// requiring only the host's OS label is admitted by an unlabelled host of
/// that OS and withheld by another OS.
fn a_host_without_the_labels_withholds_the_task_and_names_the_wait() {
    let (_root, gpu) = runtime_on(HostOs::Linux, &["gpu"]);
    let labelled = task(&gpu, "labelled", &[], &["GPU"]);
    let linux_only = task(&gpu, "linux only", &[], &["Linux"]);
    let anywhere = task(&gpu, "anywhere", &[], &[]);
    assert_eq!(labelled.requires_labels, vec!["gpu".to_string()]);
    assert_eq!(linux_only.requires_labels, vec!["linux".to_string()]);

    assert_eq!(
        admitted(&backlog(&gpu)),
        vec![
            labelled.id.clone(),
            linux_only.id.clone(),
            anywhere.id.clone()
        ]
    );
    let shown = show(&gpu, &labelled.id);
    assert_eq!(shown["requires_labels"], json!(["gpu"]), "{shown}");
    assert!(shown.get("placement_wait").is_none(), "{shown}");

    let plain = gpu.clone().with_machine_labels(Vec::new());
    let wait = "waits for a host labelled `gpu` (requires_labels); this host lacks `gpu`";
    let discovered = backlog(&plain);
    assert_eq!(
        admitted(&discovered),
        vec![linux_only.id.clone(), anywhere.id.clone()],
        "{discovered}"
    );
    let skipped = excluded(&discovered, &labelled.id).expect("the labelled task is excluded");
    assert_eq!(skipped["reason"], "host_labels_missing", "{discovered}");
    assert_eq!(skipped["detail"], wait, "{discovered}");

    let placement = "waits for a host labelled `gpu` (requires_labels): no registered serving \
                     host carries them";
    let readiness = plain.workspace_auto_readiness(&[], None, 50, &[]).unwrap();
    let entry = readiness["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["task_id"] == labelled.id.as_str())
        .expect("readiness lists the labelled task");
    assert_eq!(entry["eligible"], false, "{readiness}");
    assert_eq!(entry["reason"], "host_labels_missing", "{readiness}");
    assert_eq!(entry["detail"], wait, "{readiness}");
    assert_eq!(entry["placement_wait"], placement, "{readiness}");

    let shown = show(&plain, &labelled.id);
    assert_eq!(shown["placement_wait"], placement, "{shown}");
    let listed = plain
        .run_tool("orbit.task.list", json!({"status": "backlog"}))
        .expect("list tasks");
    let row = listed["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == labelled.id.as_str())
        .expect("the list shows the labelled task");
    assert_eq!(row["placement_wait"], placement, "{listed}");

    let refused = plain
        .submit_ship_run(
            ShipMode::Local,
            Some("main"),
            std::slice::from_ref(&labelled.id),
            CompletionPolicy::Review,
            &[],
            Some("test"),
            None,
            JobRunTrigger::cli(),
        )
        .expect_err("an unlabelled host refuses the ship");
    assert!(
        matches!(&refused, OrbitError::PolicyDenied(message) if message.contains(wait)),
        "{refused}"
    );

    let mac = plain.clone().with_host_os(Some(HostOs::Macos));
    let on_mac = backlog(&mac);
    assert_eq!(admitted(&on_mac), vec![anywhere.id], "{on_mac}");
    assert_eq!(
        excluded(&on_mac, &linux_only.id).expect("macOS withholds the linux task")["reason"],
        "host_labels_missing"
    );
}

/// `task.add` and `task.update` refuse a `requires_labels` no known host
/// carries, naming the missing labels and the known label sets, and write
/// nothing. A label set every known host lacks only in combination with the
/// task's `os:` tags is refused too. A servable requirement is stored, and an
/// empty one clears it.
fn an_unservable_label_requirement_is_refused_on_add_and_update() {
    let (root, runtime) = runtime_on(HostOs::Linux, &["xcode-cli"]);
    let workspace = root.path().join("repo").to_string_lossy().into_owned();
    let add = runtime
        .run_tool(
            "orbit.task.add",
            json!({
                "title": "Needs a GPU", "description": "Fixture.", "requires_labels": "gpu,cuda",
                "acceptance_criteria": ["Observable."], "complexity": "low", "model": "codex",
                "workspace": workspace,
            }),
        )
        .expect_err("no host carries gpu");
    assert!(
        matches!(&add, OrbitError::InvalidInput(message)
            if message.contains("no host carries `cuda`, `gpu`")
                && message.contains("`linux`, `xcode-cli`")
                && message.contains("machine.labels")),
        "{add}"
    );
    let combined = runtime
        .run_tool(
            "orbit.task.add",
            json!({
                "title": "Xcode on a Mac", "description": "Fixture.", "tags": ["os:macos"],
                "requires_labels": ["xcode-cli"], "acceptance_criteria": ["Observable."],
                "complexity": "low", "model": "codex", "workspace": workspace,
            }),
        )
        .expect_err("the only xcode-cli host runs Linux");
    assert!(
        matches!(&combined, OrbitError::InvalidInput(message)
            if message.contains("no single host carries all of them")),
        "{combined}"
    );
    assert!(
        runtime.list_tasks().unwrap().is_empty(),
        "nothing was created"
    );

    let valid = task(&runtime, "valid", &[], &[]);
    let update = runtime
        .run_tool(
            "orbit.task.update",
            json!({"id": valid.id, "requires_labels": ["gpu"], "model": "codex",
                "workspace": workspace}),
        )
        .expect_err("no host carries gpu");
    assert!(
        matches!(&update, OrbitError::InvalidInput(message) if message.contains("`gpu`")),
        "{update}"
    );
    assert!(
        runtime
            .get_task(&valid.id)
            .unwrap()
            .requires_labels
            .is_empty()
    );

    for (labels, stored) in [
        (json!(["Xcode-CLI"]), vec!["xcode-cli"]),
        (json!([]), vec![]),
    ] {
        runtime
            .run_tool(
                "orbit.task.update",
                json!({"id": valid.id, "requires_labels": labels, "model": "codex",
                    "workspace": workspace}),
            )
            .expect("a servable requirement is stored");
        assert_eq!(runtime.get_task(&valid.id).unwrap().requires_labels, stored);
    }
}

// Related boundary scenarios share the isolated table fixture.
crate::scenario_table!(
    a_host_without_the_labels_withholds_the_task_and_names_the_wait_scenarios,
    a_host_without_the_labels_withholds_the_task_and_names_the_wait,
    an_unservable_label_requirement_is_refused_on_add_and_update
);
