//! Friction stats is an observational read. A commit publishes the decided
//! envelope before it updates the task index, and holds the task's bundle
//! lock until that apply finishes. The operational metadata read waits on
//! that lock; stats must return the decided completion count without waiting.

use std::path::PathBuf;
use std::time::Duration;

use orbit_common::fs::io::{FileLockProbe, probe_file_lock, with_exclusive_file_lock};
use orbit_core::application::task::TaskAddParams;
use orbit_core::{OrbitRuntime, TaskStatus};
use orbit_store::maintenance::task_registry::task_workspaces_dir;
use serde_json::json;

const CONFIG: &str =
    "[workflow]\ndefault_crew = \"sol\"\n\n[crews.sol]\nprovider = \"codex\"\nmodel = \"test\"\n";

fn runtime() -> (OrbitRuntime, tempfile::TempDir) {
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("home/.orbit");
    let orbit = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&orbit).unwrap();
    std::fs::write(orbit.join("config.toml"), CONFIG).unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &orbit).unwrap();
    (runtime, root)
}

fn bundle_dir(runtime: &OrbitRuntime, task_id: &str) -> PathBuf {
    let root = task_workspaces_dir(&runtime.global_root());
    let workspace_id = runtime.workspace_id().unwrap();
    let direct = root.join(&workspace_id).join(task_id);
    if direct.join("task.yaml").is_file() {
        return direct;
    }
    for entry in std::fs::read_dir(&root).unwrap() {
        let candidate = entry.unwrap().path().join(task_id);
        if candidate.join("task.yaml").is_file() {
            return candidate;
        }
    }
    panic!("no bundle for {task_id} under {}", root.display());
}

/// The sibling lock file `with_exclusive_file_lock` takes for `target`.
fn lock_file(target: &std::path::Path) -> PathBuf {
    let file_name = target.file_name().unwrap().to_str().unwrap();
    let lock_path = target.with_file_name(format!(".{file_name}.lock"));
    match lock_path.parent().map(std::fs::canonicalize) {
        Some(Ok(parent)) => parent.join(format!(".{file_name}.lock")),
        _ => lock_path,
    }
}

fn tasks_done(stats: &serde_json::Value, family: &str) -> u64 {
    stats["by_family"][family]["tasks_done"]
        .as_u64()
        .unwrap_or(0)
}

fn friction_stats_reads_a_decided_commit_without_waiting_for_its_bundle_writer() {
    let (runtime, _root) = runtime();
    let task = runtime
        .add_task(TaskAddParams {
            title: "Friction stats observation fixture".into(),
            description: "Fixture task.".into(),
            acceptance_criteria: vec!["Fixture task is observable.".into()],
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .unwrap();
    let before = runtime
        .run_tool("orbit.friction.stats", json!({}))
        .expect("settled stats");
    assert_eq!(
        tasks_done(&before, "grok"),
        0,
        "a backlog task is not a completion: {before}"
    );
    let settled = runtime.list_task_metadata().unwrap();
    let observed = runtime.observe_task_metadata().unwrap();
    let projection = |tasks: &[orbit_core::Task]| {
        tasks
            .iter()
            .map(|task| (task.id.clone(), task.status, task.implemented_by.clone()))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        projection(&settled),
        projection(&observed),
        "a settled store's observational metadata matches the operational read"
    );

    let bundle_dir = bundle_dir(&runtime, &task.id);
    let envelope_path = bundle_dir.join("task.yaml");
    let lock_target = bundle_dir.with_extension("bundle");
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let writer = std::thread::spawn(move || {
        with_exclusive_file_lock(
            &lock_target,
            "friction stats fixture",
            || -> Result<(), std::io::Error> {
                assert_eq!(
                    probe_file_lock(&lock_file(&lock_target)).ok(),
                    Some(FileLockProbe::Held),
                    "the fixture must hold the bundle lock the freshness scan probes"
                );
                let mut yaml = std::fs::read_to_string(&envelope_path).unwrap();
                assert!(
                    yaml.contains("status: backlog\n"),
                    "envelope status line was not found: {yaml}"
                );
                yaml = yaml.replacen("status: backlog\n", "status: done\n", 1);
                if let Some(start) = yaml.find("implemented_by:") {
                    let end = yaml[start..]
                        .find('\n')
                        .map(|offset| start + offset)
                        .unwrap_or(yaml.len());
                    yaml.replace_range(start..end, "implemented_by: grok");
                } else {
                    yaml =
                        yaml.replacen("status: done\n", "status: done\nimplemented_by: grok\n", 1);
                }
                std::fs::write(&envelope_path, yaml).unwrap();
                ready_tx.send(()).unwrap();
                release_rx
                    .recv_timeout(Duration::from_secs(2))
                    .expect("friction stats blocked on the in-flight bundle writer");
                Ok(())
            },
        )
        .unwrap();
    });

    ready_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("writer publishes the decided envelope");
    let during = runtime
        .run_tool("orbit.friction.stats", json!({}))
        .expect("stats during apply");
    release_tx.send(()).unwrap();
    writer.join().expect("writer thread");

    assert_eq!(
        tasks_done(&during, "grok"),
        1,
        "stats must count the decided completion without waiting out the bundle writer: {during}"
    );
    assert_eq!(
        before["open"], during["open"],
        "friction counts are unchanged by the task commit: before {before} during {during}"
    );
}

crate::scenario_table!(
    friction_stats_observation,
    friction_stats_reads_a_decided_commit_without_waiting_for_its_bundle_writer,
);
