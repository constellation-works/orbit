//! Claim/bind replay and settlement delivery: lost replies, owner refusals and backoff, outages and the clock sweep.

use super::*;

/// A host that cannot spawn a claimed leaf returns its task and stops pulling
/// immediately, rather than blocking several tasks to trip the breaker.
#[test]
fn a_launch_failure_cancels_the_leaf_releases_the_task_and_suppresses_the_host() {
    if !isolated(
        module_path!(),
        "a_launch_failure_cancels_the_leaf_releases_the_task_and_suppresses_the_host",
    ) {
        return;
    }
    let pair = Pair::new(3);
    let drain = pair.start_drain();
    let failed = pair.pass(&drain);
    assert!(launch_refused(&failed), "{failed}");
    assert_eq!(
        failed["consecutive_failures"], 0,
        "a launch failure releases: {failed}"
    );
    let leaf = pair.leaf_runs().pop().unwrap();
    assert_eq!(pair.run_state(&leaf), JobRunState::Cancelled);
    assert!(
        pair.follower_jobs
            .mark_job_run_running(&leaf, Utc::now(), std::process::id())
            .is_err(),
        "a cancelled queued leaf cannot start later"
    );
    let settles = pair.wire.calls("orbit.drain.claim.settle");
    assert_eq!(settles.len(), 1, "{settles:?}");
    assert_eq!(
        settles[0]["settlement"]["Release"]["failure"]["class"],
        "environment"
    );
    assert_eq!(pair.owner_claims()[0]["claim"]["phase"], "revoked");
    assert!(
        pair.tasks
            .iter()
            .all(|task| pair.owner_status(task) == "backlog")
    );
    let suppressed = pair.pass(&drain);
    assert_eq!(suppressed["admitting"], false, "{suppressed}");
    assert!(
        suppressed["crews"]["host_suppressed"].is_string(),
        "{suppressed}"
    );
    assert_eq!(
        pair.wire.calls("orbit.task.pull").len(),
        1,
        "the systemic spawn fault is not tried on another task"
    );
}

/// An operator cancellation of a launched claimed leaf reaches its owner as a
/// release, so the task keeps its candidate in backlog with the cancel reason.
#[test]
fn operator_cancelled_claimed_leaf_returns_to_backlog_with_reason() {
    if !isolated(
        module_path!(),
        "operator_cancelled_claimed_leaf_returns_to_backlog_with_reason",
    ) {
        return;
    }
    let pair = Pair::new(1);
    let drain = pair.run_drain();
    let leaf = pair.running_leaf(&drain, 1);
    let task = pair.claimed_task(&leaf);

    // Keep this fixture from signalling the test process used as the fake
    // worker. The run is still a launched claim and exercises final settlement.
    let mut run = pair.follower_jobs.get_job_run(&leaf).unwrap().unwrap();
    run.pid = None;
    run.pid_start_time = None;
    pair.follower
        .sqlite_store()
        .unwrap()
        .upsert_job_run_for_workspace(&pair.follower.workspace_id().unwrap(), &run, None)
        .unwrap();

    let cancelled = pair
        .follower
        .cancel_job_run_with_options_and_policy(
            &leaf,
            "operator",
            "cli",
            Some("preserve this candidate for later"),
            false,
            false,
        )
        .expect("cancel claimed leaf");
    assert_eq!(cancelled.outcome, "cancelled");
    assert_eq!(pair.run_state(&leaf), JobRunState::Cancelled);
    assert_eq!(pair.owner_status(&task), "backlog");
    assert!(
        comments_of(&pair.owner_task(&task)).contains("preserve this candidate for later"),
        "{}",
        pair.owner_task(&task)
    );
    let claim = pair.follower.pull_leaf_claim(&leaf).unwrap().unwrap();
    assert_eq!(claim.settlement_phase, "settled");
}

/// A lost pull reply and then a lost bind reply are both retried under the
/// identity the owner already committed: one request, one claim, one leaf,
/// bound and launched once. A drain that gave up on the unanswered request
/// would pull the owner's second task as well.
#[test]
fn lost_pull_and_bind_replies_recover_the_same_claim_and_leaf_exactly_once() {
    if !isolated(
        module_path!(),
        "lost_pull_and_bind_replies_recover_the_same_claim_and_leaf_exactly_once",
    ) {
        return;
    }
    let pair = Pair::new(2);
    let drain = pair.start_drain();

    pair.wire.lose_next_reply("orbit.task.pull");
    let lost_pull = pair.pass(&drain);
    assert!(error_of(&lost_pull).contains("dropped"), "{lost_pull}");
    assert_eq!(pair.owner_claims().len(), 1, "the owner committed the pull");
    assert!(pair.leaf_runs().is_empty());

    pair.wire.lose_next_reply("orbit.drain.claim.bind");
    let lost_bind = pair.pass(&drain);
    assert!(error_of(&lost_bind).contains("dropped"), "{lost_bind}");
    let pulls = pair.wire.calls("orbit.task.pull");
    assert_eq!(pulls.len(), 2, "{pulls:?}");
    assert_eq!(
        pulls[0]["request_id"], pulls[1]["request_id"],
        "the unanswered request is re-sent, never replaced"
    );

    let recovered = pair.pass(&drain);
    assert!(launch_refused(&recovered), "{recovered}");
    let binds = pair.wire.calls("orbit.drain.claim.bind");
    assert_eq!(binds.len(), 2, "{binds:?}");
    assert_eq!(binds[0], binds[1], "the lost bind is replayed unchanged");

    let claims = pair.owner_claims();
    assert_eq!(claims.len(), 1, "{claims:#?}");
    let leaves = pair.leaf_runs();
    assert_eq!(leaves.len(), 1, "one leaf for the one claim: {leaves:?}");
    assert_eq!(claims[0]["bound_run"]["run_id"], leaves[0].as_str());
    assert_eq!(claims[0]["claim"]["phase"], "revoked");
    let settles = pair.wire.calls("orbit.drain.claim.settle");
    assert_eq!(settles.len(), 1, "{settles:?}");
    assert!(
        settles[0]["settlement"]["Release"]["failure"]["reason"]
            .as_str()
            .is_some_and(|summary| summary.starts_with("leaf launch failed")),
        "the bound leaf reached its launch: {settles:?}"
    );
    let claimed = claims[0]["claim"]["task_id"].as_str().unwrap();
    assert_eq!(pair.owner_status(claimed), "backlog");
    assert_eq!(pair.run_state(&leaves[0]), JobRunState::Cancelled);
    assert_eq!(
        settles[0]["settlement"]["Release"]["failure"]["class"],
        "environment"
    );
    let untouched = pair.tasks.iter().find(|id| *id != claimed).unwrap();
    assert_eq!(pair.owner_status(untouched), "backlog");
}

/// A settlement whose reply is lost stays recorded and is delivered again;
/// the owner replays the outcome it already applied rather than applying it
/// twice or refusing it, and a further replay changes nothing.
#[test]
fn a_settlement_whose_reply_is_lost_is_redelivered_and_applied_once() {
    if !isolated(
        module_path!(),
        "a_settlement_whose_reply_is_lost_is_redelivered_and_applied_once",
    ) {
        return;
    }
    let pair = Pair::new(1);
    let drain = pair.start_drain();
    let task = pair.tasks[0].clone();

    // Both cancellation's terminal hook and the launch pass can deliver.
    // Lose both replies to leave the release pending for the next pass.
    pair.wire.lose_next_reply("orbit.drain.claim.settle");
    pair.wire.lose_next_reply("orbit.drain.claim.settle");
    let lost = pair.pass(&drain);
    assert!(error_of(&lost).contains("dropped"), "{lost}");
    assert_eq!(
        pair.owner_status(&task),
        "backlog",
        "the owner applied the release"
    );
    assert_eq!(pair.follower.pending_pull_settlements().unwrap().count, 1);
    let applied = pair.owner_task(&task);

    let redelivered = pair.pass(&drain);
    assert!(redelivered["error"].is_null(), "{redelivered}");
    let settles = pair.wire.calls("orbit.drain.claim.settle");
    assert_eq!(settles.len(), 3);
    assert!(
        settles.iter().all(|settlement| *settlement == settles[0]),
        "the recorded settlement, re-sent unchanged"
    );
    assert_eq!(pair.follower.pending_pull_settlements().unwrap().count, 0);
    let leaf = pair.leaf_runs().pop().expect("leaf");
    let claim = pair
        .follower
        .pull_leaf_claim(&leaf)
        .unwrap()
        .expect("claimed leaf");
    assert_eq!(claim.settlement_phase, "settled");
    assert_eq!(claim.refusal, None, "delivered, not closed as obsolete");

    let replay = pair
        .wire
        .call("", "orbit.drain.claim.settle", settles[0].clone())
        .expect("a replayed settlement answers with the recorded outcome");
    assert_eq!(replay["phase"], "revoked", "{replay}");
    let after = pair.owner_task(&task);
    for field in ["status", "execution_summary", "comments", "history"] {
        assert_eq!(after[field], applied[field], "{field} changed on replay");
    }
    let blocked = after["history"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| event["to_status"] == "blocked")
        .count();
    assert_eq!(
        blocked, 0,
        "a launch failure never blocks the task: {after:#}"
    );
}

/// The refusal an owner answers a handoff with when its footprint widens onto
/// a path the owner's policy protects, until an operator changes that policy.
const PROTECTED_PATH_WIDENING: &str =
    "footprint widening refused protected path: .orbit/config.toml";

impl Pair {
    /// Let `leaf`'s settlement backoff elapse, as the clock passing it would.
    fn elapse_settlement_backoff(&self, leaf: &str) -> SettlementRefusal {
        let refused = self
            .admission(leaf)
            .settlement_refusal
            .expect("a refused settlement");
        self.advance(
            leaf,
            LocalPullMutation::DeferSettlement(SettlementRefusal {
                retry_after: Utc::now() - chrono::Duration::seconds(1),
                ..refused.clone()
            }),
        );
        refused
    }
}

/// [ORB-13979] An owner that refuses a handoff while it keeps the claim is
/// answering, not unreachable, and answers the same until an operator changes
/// it. The follower records the refusal once, requests no new claim, and
/// backs off — doubling to a 15 minute cap — instead of asking on every pass.
/// Once the owner accepts, the same recorded handoff settles and requests
/// resume.
#[test]
fn a_settlement_the_owner_refuses_while_holding_its_claim_backs_off_until_it_accepts() {
    if !isolated(
        module_path!(),
        "a_settlement_the_owner_refuses_while_holding_its_claim_backs_off_until_it_accepts",
    ) {
        return;
    }
    let pair = Pair::new(2);
    let drain = pair.run_drain();
    let leaf = pair.running_leaf(&drain, 1);
    pair.leaf_hands_off(&leaf);
    *pair.wire.refuse_settle.lock().unwrap() = Some(PROTECTED_PATH_WIDENING.into());
    let pulls = pair.wire.calls("orbit.task.pull").len();

    for _ in 0..5 {
        let pass = pair.pass(&drain);
        assert!(
            pass["error"].is_null(),
            "a refusal is not a failed pass: {pass}"
        );
        assert_eq!(pass["admitting"], false, "{pass}");
        assert_eq!(pass["settlement_refused"], 1, "{pass}");
        assert_eq!(pass["degraded"], false, "{pass}");
        assert!(
            error_of(&pass).is_empty()
                && pass["refusal"]
                    .as_str()
                    .is_some_and(|refusal| refusal.starts_with("settlement_refused:")),
            "{pass}"
        );
    }
    pair.follower.deliver_recorded_pull_settlements();
    assert_eq!(
        pair.wire.calls("orbit.drain.claim.settle").len(),
        1,
        "neither the drain's passes nor the clock sweep ask again before the backoff"
    );
    assert_eq!(
        pair.wire.calls("orbit.task.pull").len(),
        pulls,
        "no new claim is requested while the owner refuses what is owed"
    );
    let refused = pair
        .follower
        .pull_drain_refused_settlements(&drain)
        .unwrap();
    assert_eq!(refused.len(), 1, "{refused:?}");
    assert!(
        refused[0].reason.contains("refused protected path"),
        "{refused:?}"
    );
    assert!(!refused[0].remedy.is_empty());
    let claim = pair.follower.pull_leaf_claim(&leaf).unwrap().unwrap();
    assert_eq!(claim.settlement_phase, "settling");
    assert_eq!(claim.settlement_refusal.as_ref(), Some(&refused[0]));

    // Each refusal after the backoff elapses doubles it, up to the cap.
    let mut waits = Vec::new();
    for _ in 0..6 {
        let refused = pair.elapse_settlement_backoff(&leaf);
        waits.push((refused.retry_after - refused.last_refused_at).num_seconds());
        let pass = pair.pass(&drain);
        assert!(pass["error"].is_null(), "{pass}");
    }
    assert_eq!(waits, [60, 120, 240, 480, 900, 900]);
    assert_eq!(pair.wire.calls("orbit.drain.claim.settle").len(), 7);

    *pair.wire.refuse_settle.lock().unwrap() = None;
    pair.elapse_settlement_backoff(&leaf);
    let accepted = pair.pass(&drain);
    assert!(accepted["error"].is_null(), "{accepted}");
    assert_eq!(accepted["settlement_refused"], 0, "{accepted}");
    let settles = pair.wire.calls("orbit.drain.claim.settle");
    assert_eq!(settles.len(), 8);
    assert!(
        settles.iter().all(|settle| *settle == settles[0]),
        "the recorded handoff is re-sent unchanged"
    );
    assert!(settles[0]["settlement"].get("AcceptHandoff").is_some());
    let claim = pair.follower.pull_leaf_claim(&leaf).unwrap().unwrap();
    assert_eq!(claim.settlement_phase, "settled");
    assert_eq!(claim.settlement_refusal, None);
    assert!(
        pair.follower
            .pull_drain_refused_settlements(&drain)
            .unwrap()
            .is_empty()
    );
    pair.pass(&drain);
    assert!(
        pair.wire.calls("orbit.task.pull").len() > pulls,
        "requests resume once nothing owed is refused"
    );
}

/// [ORB-13979] An operator who fixed the owner need not wait out the
/// backoff: `orbit run auto --stop` delivers a refused settlement at once.
#[test]
fn an_operator_stop_delivers_a_refused_settlement_without_waiting_for_its_backoff() {
    if !isolated(
        module_path!(),
        "an_operator_stop_delivers_a_refused_settlement_without_waiting_for_its_backoff",
    ) {
        return;
    }
    let pair = Pair::new(1);
    let drain = pair.run_drain();
    let leaf = pair.running_leaf(&drain, 1);
    pair.leaf_hands_off(&leaf);
    *pair.wire.refuse_settle.lock().unwrap() = Some(PROTECTED_PATH_WIDENING.into());
    pair.pass(&drain);
    assert_eq!(pair.wire.calls("orbit.drain.claim.settle").len(), 1);

    *pair.wire.refuse_settle.lock().unwrap() = None;
    let stopped = pair
        .follower
        .run_tool_with_context_and_role(
            "orbit.workflow.auto",
            json!({
                "workspace": pair.follower.workspace_id().unwrap(),
                "action": "stop",
            }),
            Role::Admin,
            ToolContext {
                session_context: ToolSessionContext {
                    transport: Some(McpTransport::Local),
                    effective_capabilities: BTreeSet::from([McpCapability::Operator]),
                    ..ToolSessionContext::default()
                },
                ..ToolContext::default()
            },
        )
        .expect("stop");
    assert_eq!(
        pair.wire.calls("orbit.drain.claim.settle").len(),
        2,
        "{stopped:#}"
    );
    let claim = pair.follower.pull_leaf_claim(&leaf).unwrap().unwrap();
    assert_eq!(claim.settlement_phase, "settled", "{stopped:#}");
}

/// [ORB-13892] A settlement the owner could not take while it was down stays
/// pending, and the cancelling drain waits for it rather than ending; once
/// the owner answers again, the drain's next pass delivers it and the drain
/// ends. No new drain is needed.
#[test]
fn a_pending_settlement_is_retried_after_an_owner_outage_before_the_drain_ends() {
    if !isolated(
        module_path!(),
        "a_pending_settlement_is_retried_after_an_owner_outage_before_the_drain_ends",
    ) {
        return;
    }
    let pair = Pair::new(2);
    let drain = pair.run_drain();
    let leaf = pair.running_leaf(&drain, 1);
    let cancel = pair
        .follower
        .cancel_job_run_with_options(&drain, "operator", "cli", None, false)
        .expect("cancel");
    assert_eq!(cancel.outcome, "cancelling");

    pair.leaf_hands_off(&leaf);
    *pair.wire.unreachable.lock().unwrap() = true;
    for _ in 0..3 {
        pair.pass(&drain);
    }
    let outage = pair.pass(&drain);
    assert_eq!(outage["degraded"], true, "{outage}");
    assert_eq!(outage["done"], false, "{outage}");
    assert!(
        error_of(&outage).contains("Connection timed out"),
        "{outage}"
    );
    assert_eq!(pair.run_state(&drain), JobRunState::Running);
    assert_eq!(pair.follower.pending_pull_settlements().unwrap().count, 1);

    *pair.wire.unreachable.lock().unwrap() = false;
    let recovered = pair.pass(&drain);
    assert_eq!(recovered["done"], true, "{recovered}");
    assert_eq!(pair.follower.pending_pull_settlements().unwrap().count, 0);
    assert_eq!(pair.run_state(&drain), JobRunState::Cancelled);
    assert_eq!(
        pair.wire.calls("orbit.task.pull").len(),
        1,
        "no new request"
    );
    let claim = pair
        .follower
        .pull_leaf_claim(&leaf)
        .unwrap()
        .expect("claimed leaf");
    assert_eq!(claim.settlement_phase, "settled");
    assert_eq!(claim.refusal, None, "delivered, not closed as obsolete");
}

/// Discovery for a follower host with one replica checkout and no owner
/// checkout: nothing to schedule, only settlements to deliver.
struct ReplicaOnly(OrbitRuntime);

impl RoutineWorkspaceProvider for ReplicaOnly {
    fn discover_workspaces(&self, _: &Path) -> Result<DiscoveredWorkspaces, OrbitError> {
        let workspace = Workspace {
            id: self.0.workspace_id()?,
            name: "replica".into(),
            owner_machine_id: Some(OWNER.into()),
            git_remote: None,
            ship_mode: None,
            base_branch: "main".into(),
            status: WorkspaceStatus::Active,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        Ok(DiscoveredWorkspaces {
            replicas: vec![(workspace, self.0.clone())],
            ..DiscoveredWorkspaces::default()
        })
    }
}

/// [ORB-13892] A forced release the owner was down for is retried by the
/// clock sweep once the leaf's worker and the drain are both gone: no new
/// drain, and the task returns to the owner's backlog.
#[test]
fn a_forced_release_the_owner_missed_is_delivered_by_the_clock_sweep() {
    if !isolated(
        module_path!(),
        "a_forced_release_the_owner_missed_is_delivered_by_the_clock_sweep",
    ) {
        return;
    }
    let pair = Pair::new(1);
    let drain_worker = Worker::spawn();
    let drain = pair.run_drain_with_worker(drain_worker.pid);
    let (leaf, worker) = pair.running_leaf_with_worker(&drain, 1);
    let task = pair.claimed_task(&leaf);

    *pair.wire.unreachable.lock().unwrap() = true;
    let cancel = pair
        .follower
        .cancel_job_run_with_options(&drain, "operator", "cli", Some("host maintenance"), true)
        .expect("forced cancel");
    assert_eq!(cancel.forced_runs, vec![leaf.clone()]);
    assert!(worker.stopped());
    assert_eq!(pair.run_state(&leaf), JobRunState::Cancelled);
    assert_eq!(pair.follower.pending_pull_settlements().unwrap().count, 1);
    assert_eq!(pair.owner_status(&task), "in-progress");

    *pair.wire.unreachable.lock().unwrap() = false;
    let sweep = run_sweep_at_with_providers(
        &pair.follower.global_root(),
        SweepOptions::default(),
        RoutineMachineIdentity {
            machine_id: FOLLOWER.into(),
            machine_name: "follower".into(),
        },
        &ReplicaOnly(pair.follower.clone()),
    )
    .expect("sweep");
    assert!(!sweep.lock_busy);
    assert_eq!(pair.follower.pending_pull_settlements().unwrap().count, 0);
    let owner = pair.owner_task(&task);
    assert_eq!(owner["status"], "backlog", "{owner:#}");
    assert!(
        comments_of(&owner).contains("host maintenance"),
        "{owner:#}"
    );
    let settles = pair.wire.calls("orbit.drain.claim.settle");
    assert!(
        settles
            .iter()
            .all(|settle| settle["settlement"].get("Release").is_some()),
        "{settles:?}"
    );
    assert_eq!(
        pair.wire.calls("orbit.task.pull").len(),
        1,
        "no new request"
    );
    assert_eq!(
        pair.follower_jobs
            .list_job_runs("workspace_pull_pipeline")
            .unwrap()
            .len(),
        1,
        "no new drain"
    );
}
