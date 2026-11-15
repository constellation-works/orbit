//! A shared boundary holder deterministically outlasts pilot admission.

use std::os::unix::fs::PermissionsExt;
use std::sync::mpsc;
use std::time::Duration;

use orbit_common::fs::io::{FileLockOptions, with_shared_file_lock_options};
use orbit_types::resource::ExecutorResource;
use orbit_types::workflow::{ExecutorDef, ExecutorSandboxKind};

use super::*;

/// Ordinary readers can proceed, but the pilot's exclusive reservation cannot.
struct BoundaryHolder {
    release: Option<mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl BoundaryHolder {
    fn acquire(root: &Path) -> Self {
        let mut pending = vec![root.to_path_buf()];
        let mut target = None;
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    pending.push(path);
                } else if path
                    .file_name()
                    .is_some_and(|name| name == ".task-commit.lock")
                {
                    target = Some(path.with_file_name("task-commit"));
                }
            }
        }
        let target = target.expect("task partition boundary exists after seeding");
        let (ready_tx, ready) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            with_shared_file_lock_options(
                &target,
                "task commit boundary: ordinary (pilot fixture)",
                FileLockOptions {
                    record_shared_holders: true,
                    prefer_exclusive_waiters: true,
                    ..Default::default()
                },
                || -> Result<(), std::io::Error> {
                    ready_tx.send(()).unwrap();
                    // Sender drop also releases the holder if an assertion panics.
                    let _ = released.recv_timeout(Duration::from_secs(60));
                    Ok(())
                },
            )
            .unwrap();
        });
        ready.recv_timeout(Duration::from_secs(10)).unwrap();
        Self {
            release: Some(release),
            thread: Some(thread),
        }
    }
}

impl Drop for BoundaryHolder {
    fn drop(&mut self) {
        self.release.take();
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}

fn pilot_reservation_contention_is_held_before_agent_work() {
    let workspace = Workspace::new();
    let held = workspace.task("reserved by a live pilot");
    let free = workspace.task("waiting for admission");
    let holder_run = workspace.live_run();
    assert_eq!(
        workspace.prepare_in_run(&holder_run, &[&held.id])["task_count"],
        1
    );
    let reservation_path = workspace
        .runtime
        .paths()
        .state_dir
        .join("task-pilot-reservations.json");
    let before = std::fs::read(&reservation_path).unwrap();
    let waiter = workspace.live_run();
    let calls = workspace.root.path().join("provider-calls");
    let provider = workspace.root.path().join("codex");
    std::fs::write(&calls, "").unwrap();
    std::fs::write(
        &provider,
        format!(
            "#!/bin/sh\nprintf 'call\\n' >> '{}'\nexit 1\n",
            calls.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&provider, std::fs::Permissions::from_mode(0o755)).unwrap();
    let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
    let mut executor: ExecutorResource = serde_yaml::from_str(
        &std::fs::read_to_string(assets.join("executors/codex.yaml")).unwrap(),
    )
    .unwrap();
    executor.spec.command = Some(provider.display().to_string());
    executor.spec.sandbox = Some(ExecutorSandboxKind::Off);
    workspace
        .runtime
        .upsert_executor_def(&ExecutorDef::from_resource_spec(
            executor.metadata.name,
            executor.spec,
            None,
            None,
        ))
        .unwrap();

    let holder = BoundaryHolder::acquire(workspace.root.path());
    let input = json!({"task_ids": [held.id, free.id], "workspace_path": workspace.repo, "base_branch": "main", "run_id": waiter});
    let prepared =
        orbit_core::test_support::with_task_commit_lock_timeout(Duration::from_millis(250), || {
            workspace.runtime.run_deterministic(
                "prepare_task_pilot",
                &json!({}),
                &input,
                ToolContext::default(),
            )
        })
        .expect("a pre-operation reservation lock timeout must hold admission");
    assert_eq!(prepared["task_count"], 0, "{prepared}");
    assert_eq!(prepared["partitions"], json!([]));
    assert_eq!(
        prepared["excluded_by_reason"]["pilot_reservation_lock_busy"],
        1
    );
    assert_eq!(prepared["excluded_by_reason"]["already_preparing"], 1);
    let reason = prepared["reservation_deferred"]["reason"].as_str().unwrap();
    assert!(
        reason.contains("task commit boundary: admission") && reason.contains(".task-commit.lock"),
        "{reason}"
    );
    assert_eq!(prepared["reservation_deferred"]["code"], "lock_busy");
    assert_eq!(std::fs::read(&reservation_path).unwrap(), before);

    let run =
        orbit_core::test_support::with_task_commit_lock_timeout(Duration::from_millis(250), || {
            workspace
                .runtime
                .run_job_v2_from_yaml(&assets.join("jobs/task_pilot_pipeline.yaml"), input)
        })
        .expect("the shipped pilot pipeline settles the admission wait");
    assert!(run.success, "{run:?}");
    assert_eq!(run.pipeline["prepare"]["partition_count"], 0);
    assert_eq!(run.pipeline["apply"]["status"], "succeeded");
    assert!(
        run.pipeline["apply"]["task_outcomes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|outcome| {
                outcome["task_id"] == free.id
                    && outcome["outcome"] == "superseded"
                    && outcome["reason"] == "pilot_reservation_lock_busy"
            })
    );
    assert_eq!(
        std::fs::read_to_string(&calls).unwrap(),
        "",
        "no pilot agent launched"
    );
    assert_eq!(std::fs::read(&reservation_path).unwrap(), before);
    // A CI-sweep child has exactly one filed task. The admission hold must
    // survive its apply authority check as well, with no assessment writes.
    let sweep = orbit_core::test_support::with_task_commit_lock_timeout(
        Duration::from_millis(250),
        || workspace.runtime.run_job_v2_from_yaml(
            &assets.join("jobs/task_pilot_pipeline.yaml"),
            json!({"task_ids": [free.id], "workspace_path": workspace.repo,
                "base_branch": "main", "promotion_authorized": true,
                "ci_sweep_filing": {"task_id": free.id, "failure_key": "fixture-key",
                    "tested_commit": workspace.object("HEAD"), "workflow": "ci", "job": "test",
                    "step": "cargo test", "run_urls": ["https://github.com/example/repo/actions/runs/1"]}}),
        ),
    ).expect("a CI-sweep child's admission wait must not fail its apply");
    assert!(sweep.success, "{sweep:?}");
    assert_eq!(
        sweep.pipeline["apply"]["task_outcomes"][0]["reason"],
        "pilot_reservation_lock_busy"
    );
    assert_eq!(sweep.pipeline["apply"]["ci_sweep_admission"], json!([]));
    assert_eq!(std::fs::read_to_string(&calls).unwrap(), "");
    assert_eq!(std::fs::read(&reservation_path).unwrap(), before);
    drop(holder);

    // A later fresh prepare reserves only the free task; the other live
    // pilot's reservation survives both the wait and successful admission.
    let admitted = workspace.prepare_in_run(&waiter, &[&held.id, &free.id]);
    assert_eq!(admitted["task_ids"], json!([free.id]));
    assert_eq!(
        admitted["excluded"][0]["prepared_by_run_ids"],
        json!([holder_run])
    );
    let reservations: Value =
        serde_json::from_slice(&std::fs::read(&reservation_path).unwrap()).unwrap();
    assert_eq!(reservations["runs"][&holder_run], json!([held.id]));
    assert_eq!(reservations["runs"][&waiter], json!([free.id]));

    // A genuine reservation write error still fails. Read access remains
    // available, so this reaches reserve rather than failing discovery.
    let state_dir = workspace.runtime.paths().state_dir.clone();
    let permissions = std::fs::metadata(&state_dir).unwrap().permissions();
    std::fs::set_permissions(&state_dir, std::fs::Permissions::from_mode(0o500)).unwrap();
    let failed = workspace.runtime.run_deterministic(
        "prepare_task_pilot", &json!({}),
        &json!({"task_ids": [free.id], "workspace_path": workspace.repo, "base_branch": "main", "run_id": waiter}),
        ToolContext::default(),
    );
    std::fs::set_permissions(&state_dir, permissions).unwrap();
    let error = failed
        .expect_err("reservation I/O failure must not become a hold")
        .to_string();
    assert!(
        error.contains("reserve pilot tasks:") && error.contains("Permission denied"),
        "{error}"
    );
    assert_eq!(std::fs::read_to_string(&calls).unwrap(), "");
    assert_eq!(workspace.runtime.get_task(&held.id).unwrap(), held);
    assert_eq!(workspace.runtime.get_task(&free.id).unwrap(), free);

    // The state scheduler must observe the held member afresh, rather than
    // certifying a missing assessment as a permanent failure at this material.
    workspace
        .jobs
        .finalize_job_run(&waiter, JobRunState::Success, Utc::now(), None)
        .unwrap();
    workspace.install_pilot_job();
    let now = Utc::now();
    let routine = pilot_routine();
    evaluate_routine(&workspace.runtime, &routine, false, now).unwrap();
    let attempt = workspace.admitted(&free, 1);
    let claim_run = attempt.action_id.as_deref().unwrap();
    let holder = BoundaryHolder::acquire(workspace.root.path());
    let prepared =
        orbit_core::test_support::with_task_commit_lock_timeout(Duration::from_millis(250), || {
            super::source_moves::prepare_claim(&workspace, &attempt)
        });
    drop(holder);
    let applied = super::source_moves::apply_claim(&workspace, &attempt, &prepared);
    assert_eq!(applied["status"], "succeeded", "{applied}");
    assert_eq!(
        applied["task_outcomes"][0]["reason"],
        "pilot_reservation_lock_busy"
    );
    workspace.record_steps(claim_run, &[("prepare", &prepared), ("apply", &applied)]);
    workspace
        .jobs
        .mark_job_run_running(claim_run, now, std::process::id())
        .unwrap();
    workspace
        .jobs
        .finalize_job_run(claim_run, JobRunState::Success, Utc::now(), None)
        .unwrap();
    let observed = evaluate_routine(
        &workspace.runtime,
        &routine,
        false,
        now + chrono::Duration::minutes(3),
    )
    .unwrap();
    let members = observed.state.unwrap().members.unwrap();
    assert!(members.active.is_none());
    assert!(members.failed.is_empty(), "{:?}", members.failed);
    assert!(members.pending.contains_key(&free.id), "{members:?}");
    assert_eq!(std::fs::read_to_string(&calls).unwrap(), "");
}

crate::scenario_table!(
    pilot_reservation_contention_scenarios,
    pilot_reservation_contention_is_held_before_agent_work
);
