//! A drain and its claimed leaves whose processes all died at once, as when a
//! host OOM took the user systemd manager and every worker scope with it
//! (2026-10-12) [ORB-15783]. An operator's reconciling read — `orbit run
//! history` lists, then settles orphaned pull admissions — ends them and
//! returns their claims to the owner without a cancel.

use super::*;
use orbit_core::application::job::JobRunListParams;

/// Old enough that reconciliation's startup grace no longer shields it.
fn long_ago() -> chrono::DateTime<Utc> {
    Utc::now() - chrono::Duration::minutes(5)
}

impl Pair {
    /// A running drain whose worker is `worker`, started before the
    /// startup grace.
    fn drain_owned_by(&self, worker: &Worker) -> String {
        let drain = self.start_drain();
        self.follower_jobs
            .mark_job_run_running(&drain, long_ago(), worker.pid)
            .expect("drain running");
        drain
    }

    /// A launched claimed leaf whose worker is `worker`.
    fn leaf_owned_by(&self, drain: &str, worker: &Worker) -> String {
        let leaf = self.queued_leaf(drain, 2);
        self.advance(&leaf, LocalPullMutation::Bound);
        self.advance(&leaf, LocalPullMutation::LaunchIntent);
        self.follower_jobs
            .mark_job_run_running(&leaf, long_ago(), worker.pid)
            .expect("leaf running");
        self.advance(&leaf, LocalPullMutation::Launched);
        leaf
    }

    /// `orbit run history`: the reconciling list, then the settlement of
    /// what it left orphaned.
    fn run_history(&self) -> Vec<orbit_core::PullSettlementEntry> {
        self.follower
            .list_job_runs(JobRunListParams::default())
            .expect("reconciling history");
        self.follower.settle_orphaned_pulls()
    }
}

/// Kill `workers` and wait until each is reaped, as the OOM left them.
fn kill(workers: &[&Worker]) {
    for worker in workers {
        let _ = std::process::Command::new("kill")
            .args(["-9", &worker.pid.to_string()])
            .status();
        assert!(worker.stopped(), "worker {} did not die", worker.pid);
    }
}

/// A launched claimed leaf and its drain whose workers both died: the history
/// read interrupts both, and the leaf's failure reaches the owner, which no
/// longer holds the task as running.
fn a_dead_drain_and_claimed_leaf_are_interrupted_and_settled_with_the_owner() {
    let pair = Pair::new(1);
    let drain_worker = Worker::spawn();
    let leaf_worker = Worker::spawn();
    let drain = pair.drain_owned_by(&drain_worker);
    let leaf = pair.leaf_owned_by(&drain, &leaf_worker);
    let task = pair.claimed_task(&leaf);
    assert_eq!(
        pair.owner_claim_for_leaf(&leaf)["claim"]["phase"],
        "running"
    );
    kill(&[&drain_worker, &leaf_worker]);

    let entries = pair.run_history();

    assert_eq!(pair.run_state(&drain), JobRunState::Interrupted);
    assert_eq!(pair.run_state(&leaf), JobRunState::Interrupted);
    let settles = pair.wire.calls("orbit.drain.claim.settle");
    assert_eq!(settles.len(), 1, "{settles:?} {entries:?}");
    assert_eq!(pair.admission(&leaf).phase, LocalPullPhase::Settled);
    assert_ne!(
        pair.owner_claim_for_leaf(&leaf)["claim"]["phase"],
        "running",
        "the owner must not keep the dead leaf's claim"
    );
    assert_ne!(pair.owner_status(&task), "in-progress");

    // A second read finds nothing left to settle and contacts no owner.
    pair.run_history();
    assert_eq!(pair.wire.calls("orbit.drain.claim.settle").len(), 1);
}

/// The drain itself died before launching what it admitted: the history
/// read interrupts it and releases the admitted claim to the owner's backlog.
fn a_dead_drain_releases_the_claim_it_never_launched() {
    let pair = Pair::new(1);
    let drain_worker = Worker::spawn();
    let drain = pair.drain_owned_by(&drain_worker);
    let queued = pair.queued_leaf(&drain, 1);
    let task = pair.claimed_task(&queued);
    assert!(pair.owner_has_claim_for_task(&task));
    kill(&[&drain_worker]);

    let entries = pair.run_history();

    assert_eq!(pair.run_state(&drain), JobRunState::Interrupted);
    assert!(
        pair.admission(&queued).phase == LocalPullPhase::Settled,
        "{:?} {entries:?}",
        pair.admission(&queued).phase
    );
    assert_eq!(pair.owner_status(&task), "backlog");
    assert_ne!(
        pair.owner_claim_for_leaf(&queued)["claim"]["phase"],
        "running"
    );
}

/// A drain and leaf whose workers are alive but slow are left running: the
/// read reconciles nothing and settles nothing with the owner.
fn a_live_slow_drain_and_leaf_are_not_reconciled() {
    let pair = Pair::new(1);
    let drain_worker = Worker::spawn();
    let leaf_worker = Worker::spawn();
    let drain = pair.drain_owned_by(&drain_worker);
    let leaf = pair.leaf_owned_by(&drain, &leaf_worker);
    let task = pair.claimed_task(&leaf);

    let entries = pair.run_history();

    assert!(entries.is_empty(), "{entries:?}");
    assert_eq!(pair.run_state(&drain), JobRunState::Running);
    assert_eq!(pair.run_state(&leaf), JobRunState::Running);
    assert!(pair.wire.calls("orbit.drain.claim.settle").is_empty());
    assert_eq!(pair.admission(&leaf).phase, LocalPullPhase::Launched);
    assert_eq!(
        pair.owner_claim_for_leaf(&leaf)["claim"]["phase"],
        "running"
    );
    assert_eq!(pair.owner_status(&task), "in-progress");
    assert!(drain_worker.running() && leaf_worker.running());
}

// Related boundary scenarios share the isolated table fixture.
crate::scenario_table!(
    a_dead_drain_and_claimed_leaf_are_interrupted_and_settled_with_the_owner_scenarios,
    a_dead_drain_and_claimed_leaf_are_interrupted_and_settled_with_the_owner,
    a_dead_drain_releases_the_claim_it_never_launched,
    a_live_slow_drain_and_leaf_are_not_reconciled
);
