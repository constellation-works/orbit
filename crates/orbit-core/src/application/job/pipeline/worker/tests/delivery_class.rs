//! A delivery's work class is captured once and inherited along its run
//! chain, and a worker without a scope records nothing.

use chrono::Utc;
#[cfg(target_os = "linux")]
use orbit_store::contracts::ResourceUsageStoreBackend;
use orbit_types::task::{TaskComplexity, TaskStatus};
use orbit_types::workflow::{JobRun, PipelineState};
use serde_json::{Value, json};
use tempfile::TempDir;

use crate::OrbitRuntime;
use crate::application::job::pipeline::worker::delivery_class::work_class;
use crate::application::task::{TaskAddParams, TaskUpdateParams};
use crate::runtime::host_resource::delivery::RESOURCE_CLASS_KEY;

fn tags(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| value.to_string()).collect()
}

/// The template is the most specific key, then `no-diff-expected`, then
/// everything else.
#[test]
fn work_class_prefers_template_then_no_diff_then_change() {
    for (task_tags, expected) in [
        (
            &["no-diff-expected", "auto-task:docs-audit"][..],
            "auto-task:docs-audit",
        ),
        (&["auto-task:qa-sweep"][..], "auto-task:qa-sweep"),
        (&["no-diff-expected", "os:linux"][..], "no-diff"),
        (&["auto-task:"][..], "change"),
        (&["auto-concurrency"][..], "change"),
        (&[][..], "change"),
    ] {
        assert_eq!(work_class(&tags(task_tags)), expected, "{task_tags:?}");
    }
}

fn runtime() -> (TempDir, OrbitRuntime) {
    let root = tempfile::tempdir().expect("tempdir");
    let global = root.path().join("global");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &workspace).expect("runtime");
    (root, runtime)
}

fn persisted_run(runtime: &OrbitRuntime, job: &str, input: Value) -> JobRun {
    let run = runtime
        .stores()
        .jobs()
        .insert_job_run(job, 1, Utc::now(), Some(input.clone()), None)
        .expect("insert run");
    runtime
        .write_run_state(
            &run.run_id,
            &PipelineState::new(run.run_id.clone(), run.job_id.clone(), input),
        )
        .expect("seed run state");
    run
}

fn admitted(
    runtime: &OrbitRuntime,
    job: &str,
    task_id: &str,
    parent: Option<&str>,
    retried: bool,
) -> Value {
    let mut input = json!({ "task_ids": [task_id] });
    runtime.install_resource_class_admission(job, &mut input, parent, retried);
    input
}

/// Editing the task's tags after its delivery started changes nothing for
/// that delivery's later runs; a new delivery sees the new tags.
#[test]
fn class_is_captured_once_per_delivery_and_survives_tag_edits() {
    if crate::application::tests::run_isolated_test(std::any::type_name_of_val(
        &class_is_captured_once_per_delivery_and_survives_tag_edits,
    )) {
        return;
    }
    let (_root, runtime) = runtime();
    let task = runtime
        .add_task(TaskAddParams {
            title: "Docs audit fixture".into(),
            complexity: TaskComplexity::Medium,
            status: Some(TaskStatus::Backlog),
            tags: tags(&["auto-task:docs-audit", "no-diff-expected"]),
            ..Default::default()
        })
        .expect("add task");

    let parent_input = admitted(&runtime, "task_auto_pipeline", &task.id, None, false);
    assert_eq!(
        parent_input[RESOURCE_CLASS_KEY]["work_class"],
        "auto-task:docs-audit"
    );
    assert_eq!(parent_input[RESOURCE_CLASS_KEY]["complexity"], "medium");
    assert_eq!(parent_input[RESOURCE_CLASS_KEY]["resumed"], false);
    let parent = persisted_run(&runtime, "task_auto_pipeline", parent_input.clone());

    runtime
        .update_task(
            &task.id,
            TaskUpdateParams {
                tags: Some(Vec::new()),
                ..Default::default()
            },
        )
        .expect("edit tags");

    let child_input = admitted(
        &runtime,
        "task_gate_pipeline",
        &task.id,
        Some(&parent.run_id),
        false,
    );
    assert_eq!(
        child_input[RESOURCE_CLASS_KEY], parent_input[RESOURCE_CLASS_KEY],
        "a child of the delivery inherits its class"
    );
    let child = persisted_run(&runtime, "task_pr_pipeline", child_input);
    let class = runtime
        .delivery_resource_class(&child, false)
        .expect("a delivery run has a class");
    assert_eq!(class.work_class, "auto-task:docs-audit");
    assert_eq!(class.complexity.as_deref(), Some("medium"));
    assert_eq!(
        json!(class.admitted_at),
        parent_input[RESOURCE_CLASS_KEY]["admitted_at"]
    );
    assert!(!class.resumed);

    let fresh = admitted(&runtime, "task_auto_pipeline", &task.id, None, false);
    assert_eq!(
        fresh[RESOURCE_CLASS_KEY]["work_class"], "change",
        "a new delivery"
    );

    // A caller cannot pick its own class for a fresh submission.
    let mut forged = json!({ "task_ids": [task.id] });
    forged[RESOURCE_CLASS_KEY] = json!({
        "task_id": task.id, "work_class": "no-diff", "admitted_at": Utc::now(), "resumed": false,
    });
    runtime.install_resource_class_admission("task_auto_pipeline", &mut forged, None, false);
    assert_eq!(forged[RESOURCE_CLASS_KEY]["work_class"], "change");

    // A resume keeps the class it carries and is marked for exclusion.
    let mut resumed = parent_input.clone();
    runtime.install_resource_class_admission("task_auto_pipeline", &mut resumed, None, true);
    assert_eq!(
        resumed[RESOURCE_CLASS_KEY]["work_class"],
        "auto-task:docs-audit"
    );
    assert_eq!(resumed[RESOURCE_CLASS_KEY]["resumed"], true);

    // Retried runs are told apart at the worker too.
    let retried = runtime
        .stores()
        .jobs()
        .insert_job_run(
            "task_pr_pipeline",
            1,
            Utc::now(),
            Some(parent_input.clone()),
            Some(parent.run_id.clone()),
        )
        .expect("insert retry");
    assert!(
        runtime
            .delivery_resource_class(&retried, false)
            .unwrap()
            .resumed
    );
    assert!(
        runtime
            .delivery_resource_class(&parent, true)
            .unwrap()
            .resumed,
        "adopted"
    );

    // Non-delivery jobs record nothing.
    let pilot = persisted_run(
        &runtime,
        "task_pilot_pipeline",
        json!({ "task_id": task.id }),
    );
    assert!(runtime.delivery_resource_class(&pilot, false).is_none());
}

/// A worker that resolved no scope (containment off, no user manager, macOS)
/// starts no sampler, writes no rows and logs nothing at warn or above. One
/// that resolved a scope records its minutes.
#[cfg(target_os = "linux")]
#[test]
fn worker_without_a_scope_records_nothing_and_logs_nothing() {
    if crate::application::tests::run_isolated_test(std::any::type_name_of_val(
        &worker_without_a_scope_records_nothing_and_logs_nothing,
    )) {
        return;
    }
    use crate::application::job::pipeline::worker::scope::WorkerScopeCgroup;

    let (root, runtime) = runtime();
    let task = runtime
        .add_task(TaskAddParams {
            title: "Scope fixture".into(),
            complexity: TaskComplexity::Low,
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .expect("add task");
    let input = admitted(&runtime, "task_pr_pipeline", &task.id, None, false);
    let run = persisted_run(&runtime, "task_pr_pipeline", input);
    let store = runtime.sqlite_store().unwrap();
    let workspace_id = runtime.workspace_id().unwrap();

    let log = root.path().join("warnings.log");
    let writer = std::fs::File::create(&log).unwrap();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .with_writer(move || writer.try_clone().unwrap())
        .finish();
    let sampler = tracing::subscriber::with_default(subscriber, || {
        runtime.start_resource_sampler_in(None, &run, false)
    });
    assert!(sampler.is_none());
    assert!(
        store
            .run_resource_minutes(&workspace_id, &run.run_id)
            .unwrap()
            .is_empty()
    );
    assert_eq!(std::fs::read_to_string(&log).unwrap(), "", "no warning");

    // Not in a worker scope: `/proc/self/cgroup` names a service.
    assert!(
        WorkerScopeCgroup::from_proc_cgroup(
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/orbit.service\n",
            root.path(),
        )
        .is_none()
    );

    let path = format!("app.slice/orbit-worker-{}-0000abcd.scope", run.run_id);
    let scope_dir = root.path().join("cgroup").join(&path);
    std::fs::create_dir_all(&scope_dir).unwrap();
    std::fs::write(scope_dir.join("cpu.stat"), "usage_usec 10\n").unwrap();
    std::fs::write(scope_dir.join("memory.current"), "4096\n").unwrap();
    std::fs::write(scope_dir.join("memory.stat"), "file 1024\nshmem 0\n").unwrap();
    let scope =
        WorkerScopeCgroup::from_proc_cgroup(&format!("0::/{path}\n"), &root.path().join("cgroup"));
    drop(
        runtime
            .start_resource_sampler_in(scope, &run, false)
            .expect("a scoped delivery samples"),
    );
    let rows = store
        .run_resource_minutes(&workspace_id, &run.run_id)
        .unwrap();
    assert_eq!(rows.len(), 1, "{rows:#?}");
    assert_eq!(rows[0].memory_bytes, 3072);
    assert_eq!(rows[0].task_id, task.id);
    assert_eq!(rows[0].complexity.as_deref(), Some("low"));
}
