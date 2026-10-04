//! Follower worktree garbage collection for claimed leaves.

use super::*;

/// The checkout setup gives a claimed leaf: a Git worktree of the follower's
/// checkout on the leaf's own branch, holding a Cargo `target/` that the
/// checkout ignores. Returns the worktree and the build output's size.
fn leaf_worktree(pair: &Pair, leaf: &str) -> (PathBuf, u64) {
    let repo = &pair.follower_repo;
    if !repo.join(".git").exists() {
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
    let pair = Pair::new(1);
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
    let pair = Pair::new(1);
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
    let pair = Pair::new(1);
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
