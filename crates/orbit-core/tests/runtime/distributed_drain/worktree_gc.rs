//! Follower worktree garbage collection for claimed leaves.

use super::*;

/// GC must see distinct machine namespaces, including tasks this follower
/// minted before it became a replica.
fn gc_pair(tasks: usize) -> Pair {
    use orbit_store::maintenance::task_registry::{TaskRegistryStore, task_registry_path};

    let mut pair = Pair::new(tasks);
    let global = pair.follower.global_root();
    std::fs::write(
        global.join("config.toml"),
        "[machine]\nid = \"hm_0000000000000002\"\nname = \"follower\"\ntask_prefix = \"DANI\"\n",
    )
    .unwrap();
    TaskRegistryStore::open(&task_registry_path(&global))
        .unwrap()
        .set_task_prefix("DANI")
        .unwrap();
    pair.follower = OrbitRuntime::from_roots_with_binding(
        &global,
        &pair.follower_repo.join(".orbit"),
        orbit_core::WorkspaceRuntimeBinding {
            logical_workspace_id: pair.wire.owner.workspace_id().unwrap(),
            task_partition_id: pair.follower.workspace_id().unwrap(),
            owner_machine_id: Some(OWNER.into()),
            checkout_role: None,
            repo_root: pair.follower_repo.clone(),
            ship_mode: orbit_core::ShipMode::Local,
            base_branch: None,
        },
    )
    .unwrap()
    .with_automation_machine_identity(Some(FOLLOWER.into()))
    .with_coordination_write_owner(Some(OWNER.into()))
    .with_drain_owner_transport(pair.wire.clone());
    pair
}

fn terminal_worktree(pair: &Pair, tasks: &[&str]) -> (String, PathBuf) {
    let run = pair
        .follower_jobs
        .insert_job_run(
            "task_pr_pipeline",
            1,
            Utc::now(),
            Some(json!({"task_ids": tasks, "scope": "all"})),
            None,
        )
        .unwrap();
    pair.follower_jobs
        .mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
        .unwrap();
    pair.follower_jobs
        .finalize_job_run(&run.run_id, JobRunState::Failed, Utc::now(), None)
        .unwrap();
    let (path, _) = leaf_worktree(pair, &run.run_id);
    (run.run_id, path)
}

fn local_done_task(pair: &Pair) -> String {
    let local = pair.follower.clone().with_coordination_write_owner(None);
    let id = local
        .add_task(orbit_core::application::task::TaskAddParams {
            title: "Historical local work".into(),
            description: "Completed before this checkout became a replica.".into(),
            acceptance_criteria: vec!["Work complete.".into()],
            plan: "Complete the work.".into(),
            ..Default::default()
        })
        .unwrap()
        .id;
    local
        .force_update_task_with_identity(
            &id,
            orbit_core::application::task::TaskUpdateParams {
                status: Some(orbit_types::task::TaskStatus::Done),
                ..Default::default()
            },
            None,
            None,
        )
        .unwrap();
    id
}

fn mirror_tasks(pair: &Pair, source: &OrbitRuntime) {
    use orbit_store::maintenance::task_registry::{TaskRegistryStore, task_registry_path};
    use orbit_store::workflow::task::{
        ExportSelection, ImportConflictPolicy, export_tasks, import_tasks,
    };

    let source_registry =
        TaskRegistryStore::open(&task_registry_path(&source.global_root())).unwrap();
    let target_registry =
        TaskRegistryStore::open(&task_registry_path(&pair.follower.global_root())).unwrap();
    let archive = pair._root.path().join("mirrors.tar.zst");
    export_tasks(
        &source_registry,
        &source.workspace_id().unwrap(),
        ExportSelection::All,
        &archive,
        Utc::now(),
    )
    .unwrap();
    import_tasks(
        &target_registry,
        &archive,
        Some(&pair.follower.workspace_id().unwrap()),
        ImportConflictPolicy::OwnerWins,
    )
    .unwrap();
}

/// The checkout setup gives a claimed leaf: a Git worktree of the follower's
/// checkout on the leaf's own branch, holding a Cargo `target/` that the
/// checkout ignores. Returns the worktree and the build output's size.
fn leaf_worktree(pair: &Pair, leaf: &str) -> (PathBuf, u64) {
    let repo = &pair.follower_repo;
    if git(repo, &["rev-parse", "--is-inside-work-tree"]).trim() != "true" {
        git(repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join(".gitignore"), "/target/\n/.orbit/\n").unwrap();
        git(repo, &["add", ".gitignore"]);
        git(repo, &["commit", "-q", "-m", "init"]);
    }
    let worktree = repo
        .join(".orbit/state/worktrees")
        .join(format!("orbit-{leaf}"));
    git(
        repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            &format!("orbit/{leaf}"),
            worktree.to_str().unwrap(),
        ],
    );
    let build = worktree.join("target/debug");
    std::fs::create_dir_all(&build).unwrap();
    std::fs::write(build.join("orbit"), vec![0u8; 4096]).unwrap();
    (worktree, 4096)
}

fn gc_report(result: &orbit_engine::WorktreeGcResult, leaf: &str) -> Value {
    let result = serde_json::to_value(result).unwrap();
    result["reports"]
        .as_array()
        .unwrap()
        .iter()
        .find(|report| report["run_id"] == leaf)
        .cloned()
        .unwrap_or_else(|| panic!("no report for {leaf}: {result:#}"))
}

/// [ORB-13920] An accepted handoff is all a follower needs to give back its
/// leaf's disk. The drain's next pass reclaims the leaf's `target/` and keeps
/// the checkout; worktree GC then removes the checkout on the strength of the
/// settled admission alone. Every owner task read would fail here, and none
/// is made.
#[test]
fn an_accepted_handoff_gives_back_its_build_output_and_then_its_worktree() {
    if !isolated(
        module_path!(),
        "an_accepted_handoff_gives_back_its_build_output_and_then_its_worktree",
    ) {
        return;
    }
    let pair = gc_pair(1);
    let drain = pair.run_drain();
    let (leaf, worker) = pair.running_leaf_with_worker(&drain, 1);
    pair.leaf_hands_off(&leaf);
    assert!(
        std::process::Command::new("kill")
            .args(["-TERM", &worker.pid.to_string()])
            .status()
            .unwrap()
            .success()
    );
    assert!(worker.stopped(), "the handed-off leaf's worker has exited");
    let settled = pair.pass(&drain);
    assert!(settled["error"].is_null(), "{settled}");
    assert!(matches!(
        pair.admission(&leaf).settlement,
        Some(ClaimMutation::AcceptHandoff(_))
    ));
    let claim = pair
        .follower
        .pull_leaf_claim(&leaf)
        .unwrap()
        .expect("claimed leaf");
    assert_eq!(claim.settlement_phase, "settled");
    let (worktree, build_bytes) = leaf_worktree(&pair, &leaf);
    *pair.wire.task_reads_fail.lock().unwrap() =
        Some("ssh: connect to host owner port 22: Connection timed out".into());

    let next = pair.pass(&drain);
    assert!(
        next["reclaimed_build_bytes"]
            .as_u64()
            .is_some_and(|bytes| bytes >= build_bytes),
        "{next}"
    );
    assert!(!worktree.join("target").exists(), "build output reclaimed");
    assert!(
        worktree.join(".gitignore").exists(),
        "the checkout stays for worktree GC"
    );

    let gc = pair
        .follower
        .gc_worktrees(true, None, None, false, false)
        .unwrap();
    let report = gc_report(&gc, &leaf);
    assert_eq!(report["action"], "removed", "{report:#}");
    assert!(
        report["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("claim settled")),
        "{report:#}"
    );
    assert!(!worktree.exists());
    assert!(
        pair.wire.task_reads.lock().unwrap().is_empty(),
        "an accepted handoff needs no task read or follower task records"
    );
}

/// [ORB-13950] A forced release settles the admission but returns unfinished
/// work to the owner's backlog. GC must keep the clean checkout and its
/// unhanded commits, including when the owner cannot be queried.
#[test]
fn a_released_claim_keeps_its_backlogged_worktree_and_unhanded_commit() {
    if !isolated(
        module_path!(),
        "a_released_claim_keeps_its_backlogged_worktree_and_unhanded_commit",
    ) {
        return;
    }
    let pair = gc_pair(1);
    let drain_worker = Worker::spawn();
    let drain = pair.run_drain_with_worker(drain_worker.pid);
    let (leaf, worker) = pair.running_leaf_with_worker(&drain, 1);
    let task = pair.claimed_task(&leaf);
    let (worktree, _) = leaf_worktree(&pair, &leaf);
    std::fs::write(worktree.join("unfinished.rs"), "fn unfinished() {}\n").unwrap();
    git(&worktree, &["add", "unfinished.rs"]);
    git(&worktree, &["commit", "-q", "-m", "unfinished work"]);
    let head = git(&worktree, &["rev-parse", "HEAD"]);
    assert!(git(&worktree, &["status", "--porcelain"]).is_empty());

    pair.follower
        .cancel_job_run_with_options(&drain, "operator", "cli", Some("host maintenance"), true)
        .expect("forced cancel");
    assert!(worker.stopped());
    assert_eq!(pair.run_state(&leaf), JobRunState::Cancelled);
    assert_eq!(pair.owner_status(&task), "backlog");
    let admission = pair.admission(&leaf);
    assert_eq!(admission.phase, LocalPullPhase::Settled);
    assert!(matches!(
        admission.settlement,
        Some(ClaimMutation::Release(_))
    ));

    let gc = pair
        .follower
        .gc_worktrees(true, None, None, false, false)
        .unwrap();
    let report = gc_report(&gc, &leaf);
    assert_eq!(
        report["action"], "skipped:task_status_ineligible",
        "{report:#}"
    );
    assert_eq!(report["task_status"], "backlog", "{report:#}");
    assert!(worktree.join("unfinished.rs").exists());
    assert_eq!(git(&worktree, &["rev-parse", "HEAD"]), head);
    assert_eq!(
        *pair.wire.task_reads.lock().unwrap(),
        vec![pair.destination["selector"].as_str().unwrap().to_string()],
        "a release still asks its owner over the claim's route"
    );

    *pair.wire.task_reads_fail.lock().unwrap() = Some("owner unreachable".into());
    let gc = pair
        .follower
        .gc_worktrees(true, None, None, false, false)
        .unwrap();
    let report = gc_report(&gc, &leaf);
    assert_eq!(report["action"], "skipped:owner_unreachable", "{report:#}");
    assert!(worktree.join("unfinished.rs").exists());
    assert_eq!(git(&worktree, &["rev-parse", "HEAD"]), head);
}

/// [ORB-13920] A claim not yet settled leaves the decision to its task's
/// status on the owner, read over the claim's own route. A transport failure
/// is reported as `owner_unreachable` carrying the transport's error; once
/// the owner answers, its answer decides.
#[test]
fn an_unsettled_claimed_worktree_asks_its_owner_and_reports_a_transport_failure() {
    if !isolated(
        module_path!(),
        "an_unsettled_claimed_worktree_asks_its_owner_and_reports_a_transport_failure",
    ) {
        return;
    }
    let pair = gc_pair(1);
    let drain = pair.start_drain();
    pair.wire.lose_next_reply("orbit.drain.claim.settle");
    let lost = pair.pass(&drain);
    assert!(error_of(&lost).contains("dropped"), "{lost}");
    let leaf = pair.leaf_runs().pop().expect("leaf");
    let claim = pair
        .follower
        .pull_leaf_claim(&leaf)
        .unwrap()
        .expect("claimed leaf");
    assert_eq!(claim.settlement_phase, "settling");
    // The leaf has finished; only its settlement is still owed to the owner.
    set_run_state(&pair.follower, &leaf, "running");
    pair.follower_jobs
        .finalize_job_run(
            &leaf,
            orbit_types::workflow::JobRunState::Failed,
            Utc::now(),
            None,
        )
        .unwrap();
    let (worktree, _) = leaf_worktree(&pair, &leaf);
    let timeout = "ssh: connect to host owner port 22: Connection timed out";
    *pair.wire.task_reads_fail.lock().unwrap() = Some(timeout.into());

    let unreachable = pair
        .follower
        .gc_worktrees(false, None, None, false, false)
        .unwrap();
    let report = gc_report(&unreachable, &leaf);
    assert_eq!(report["action"], "skipped:owner_unreachable", "{report:#}");
    assert!(
        report["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains(timeout)),
        "the transport's error is reported: {report:#}"
    );
    let selector = pair.destination["selector"].as_str().unwrap();
    assert_eq!(
        *pair.wire.task_reads.lock().unwrap(),
        vec![selector.to_string()],
        "asked over the claim's route"
    );

    *pair.wire.task_reads_fail.lock().unwrap() = None;
    *pair.wire.task_reads_remote_error.lock().unwrap() = Some((
        "execution_failed".into(),
        "owner task store unavailable".into(),
    ));
    let owner_error = pair
        .follower
        .gc_worktrees(false, None, None, false, false)
        .unwrap();
    let report = gc_report(&owner_error, &leaf);
    assert_eq!(
        report["action"], "skipped:owner_lookup_failed",
        "{report:#}"
    );
    assert!(
        report["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("owner task store unavailable")),
        "a structured owner error proves the route answered: {report:#}"
    );

    *pair.wire.task_reads_remote_error.lock().unwrap() = None;
    let answered = pair
        .follower
        .gc_worktrees(false, None, None, false, false)
        .unwrap();
    let report = gc_report(&answered, &leaf);
    assert_eq!(
        report["action"], "skipped:task_status_ineligible",
        "{report:#}"
    );
    assert_eq!(
        report["task_status"], "blocked",
        "the owner's answer decides: {report:#}"
    );
    assert!(worktree.exists());
}

/// A replica retains authority over its own historical tasks [ORB-14447].
#[test]
fn a_replica_collects_a_local_done_task_without_asking_the_owner() {
    if !isolated(
        module_path!(),
        "a_replica_collects_a_local_done_task_without_asking_the_owner",
    ) {
        return;
    }
    let pair = gc_pair(0);
    let task = local_done_task(&pair);
    assert_eq!(orbit_types::task::task_id_prefix(&task), Some("DANI"));
    let (leaf, path) = terminal_worktree(&pair, &[&task]);
    let missing = orbit_types::task::format_task_id("DANI", u32::MAX).unwrap();
    let (missing_leaf, missing_path) = terminal_worktree(&pair, &[&missing]);
    *pair.wire.task_reads_fail.lock().unwrap() = Some("owner unavailable".into());

    let gc = pair
        .follower
        .gc_worktrees(true, None, None, false, false)
        .unwrap();
    assert_eq!(gc_report(&gc, &leaf)["action"], "removed");
    assert!(!path.exists());
    assert_eq!(
        gc_report(&gc, &missing_leaf)["action"],
        "skipped:task_unresolved"
    );
    assert!(missing_path.exists(), "a missing local task fails closed");
    assert!(pair.wire.task_reads.lock().unwrap().is_empty());
}

/// An owner outage is memoized without poisoning other namespaces, and a
/// third prefix never causes a failed task.show call on that owner [ORB-14447].
#[test]
fn replica_gc_routes_owner_ids_and_keeps_other_prefixes_independent() {
    if !isolated(
        module_path!(),
        "replica_gc_routes_owner_ids_and_keeps_other_prefixes_independent",
    ) {
        return;
    }
    let pair = gc_pair(1);
    let drain = pair.start_drain();
    pair.pass(&drain); // Persist an admission identifying the owner's prefix.
    let owner_task = &pair.tasks[0];
    let third = orbit_types::task::format_task_id("THIRD", 1).unwrap();
    let local = local_done_task(&pair);
    let (owner_leaf, owner_path) = terminal_worktree(&pair, &[owner_task]);
    let (second_owner_leaf, _) = terminal_worktree(&pair, &[owner_task]);
    let (local_leaf, local_path) = terminal_worktree(&pair, &[&local]);
    let (third_leaf, third_path) = terminal_worktree(&pair, &[&third]);
    let selector = pair.destination["selector"].as_str().unwrap();

    *pair.wire.task_reads_fail.lock().unwrap() = Some("owner unreachable".into());
    let outage = pair
        .follower
        .gc_worktrees(true, None, None, false, false)
        .unwrap();
    for leaf in [&owner_leaf, &second_owner_leaf] {
        assert_eq!(
            gc_report(&outage, leaf)["action"],
            "skipped:owner_unreachable"
        );
    }
    assert_eq!(gc_report(&outage, &local_leaf)["action"], "removed");
    assert!(!local_path.exists());
    assert_eq!(
        gc_report(&outage, &third_leaf)["action"],
        "skipped:task_prefix_unroutable"
    );
    assert!(owner_path.exists() && third_path.exists());
    assert_eq!(
        *pair.wire.task_reads.lock().unwrap(),
        vec![selector.to_string()],
        "one owner-prefix transport failure per sweep"
    );

    pair.wire.task_reads.lock().unwrap().clear();
    *pair.wire.task_reads_fail.lock().unwrap() = None;
    for (code, action) in [
        ("unauthorized", "skipped:owner_lookup_failed"),
        ("not_found", "skipped:task_unresolved"),
    ] {
        *pair.wire.task_reads_remote_error.lock().unwrap() =
            Some((code.into(), "owner refused lookup".into()));
        let gc = pair
            .follower
            .gc_worktrees(true, None, None, false, false)
            .unwrap();
        assert_eq!(gc_report(&gc, &owner_leaf)["action"], action);
        assert_eq!(
            gc_report(&gc, &third_leaf)["action"],
            "skipped:task_prefix_unroutable"
        );
        assert!(owner_path.exists() && third_path.exists());
    }
    assert_eq!(
        *pair.wire.task_reads.lock().unwrap(),
        vec![selector.to_string(), selector.to_string()],
        "the task answer is memoized within each sweep, but retried on the next sweep"
    );
}

/// Before a pull, workspace mirrors can identify one foreign namespace.
/// Multiple foreign namespaces need an owner admission to disambiguate them.
#[test]
fn replica_gc_learns_the_owner_prefix_from_mirrors_or_claim_admissions() {
    if !isolated(
        module_path!(),
        "replica_gc_learns_the_owner_prefix_from_mirrors_or_claim_admissions",
    ) {
        return;
    }
    use orbit_store::maintenance::task_registry::{TaskRegistryStore, task_registry_path};

    let pair = gc_pair(1);
    let owner_task = &pair.tasks[0];
    let (leaf, path) = terminal_worktree(&pair, &[owner_task]);
    let unknown = pair
        .follower
        .gc_worktrees(false, None, None, false, false)
        .unwrap();
    assert_eq!(
        gc_report(&unknown, &leaf)["action"],
        "skipped:task_prefix_unroutable"
    );
    assert!(pair.wire.task_reads.lock().unwrap().is_empty());

    mirror_tasks(&pair, &pair.wire.owner);
    let mirrored = pair
        .follower
        .gc_worktrees(false, None, None, false, false)
        .unwrap();
    assert_eq!(gc_report(&mirrored, &leaf)["task_status"], "backlog");
    assert_eq!(pair.wire.task_reads.lock().unwrap().len(), 1);

    let (third, third_repo) = open_runtime(pair._root.path(), "hm_third");
    TaskRegistryStore::open(&task_registry_path(&third.global_root()))
        .unwrap()
        .set_task_prefix("THIRD")
        .unwrap();
    let third_task = backlog_task(&third, &third_repo, "src/third.rs", None);
    mirror_tasks(&pair, &third);
    let (third_leaf, third_path) = terminal_worktree(&pair, &[&third_task]);
    pair.wire.task_reads.lock().unwrap().clear();
    let ambiguous = pair
        .follower
        .gc_worktrees(false, None, None, false, false)
        .unwrap();
    for run in [&leaf, &third_leaf] {
        assert_eq!(
            gc_report(&ambiguous, run)["action"],
            "skipped:task_prefix_unroutable"
        );
    }
    assert!(pair.wire.task_reads.lock().unwrap().is_empty());

    let drain = pair.start_drain();
    pair.pass(&drain);
    let claimed = pair
        .follower
        .gc_worktrees(false, None, None, false, false)
        .unwrap();
    assert_eq!(gc_report(&claimed, &leaf)["task_status"], "blocked");
    assert_eq!(
        gc_report(&claimed, &third_leaf)["action"],
        "skipped:task_prefix_unroutable"
    );
    assert_eq!(pair.wire.task_reads.lock().unwrap().len(), 1);
    assert!(path.exists() && third_path.exists());
}
