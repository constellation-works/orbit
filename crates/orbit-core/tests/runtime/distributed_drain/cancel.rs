//! Graceful and forced drain cancel.

use super::*;

/// [ORB-13892] Cancelling a running pull drain is graceful. The request
/// returns at once naming the leaf it waits for; the drain's next pass stops
/// requesting and returns the claim it never launched to the owner's backlog
/// with a comment naming the drain, while the launched leaf keeps running.
/// Once that leaf hands off, the next pass delivers it and the drain ends
/// `cancelled`.
#[test]
fn a_graceful_drain_cancel_releases_unlaunched_claims_and_waits_for_running_leaves() {
    if !isolated(
        module_path!(),
        "a_graceful_drain_cancel_releases_unlaunched_claims_and_waits_for_running_leaves",
    ) {
        return;
    }
    let pair = Pair::new(3);
    let drain = pair.run_drain();
    let running = pair.running_leaf(&drain, 2);
    let queued = pair.queued_leaf(&drain, 2);
    let (running_task, queued_task) = (pair.claimed_task(&running), pair.claimed_task(&queued));

    let cancel = pair
        .follower
        .cancel_job_run_with_options(&drain, "operator", "cli", Some("host maintenance"), false)
        .expect("cancel");
    assert_eq!(cancel.outcome, "cancelling");
    let waiting: Vec<_> = cancel
        .waiting_leaves
        .iter()
        .map(|leaf| leaf.leaf_run_id.as_str())
        .collect();
    assert_eq!(waiting, vec![running.as_str()]);
    assert_eq!(pair.run_state(&drain), JobRunState::Running);

    let waits = pair.pass_with(&drain, 2);
    assert_eq!(waits["cancelling"], true, "{waits}");
    assert_eq!(waits["done"], false, "{waits}");
    assert!(waits["error"].is_null(), "{waits}");
    assert_eq!(
        waits["waiting_leaves"][0]["leaf_run_id"],
        running.as_str(),
        "{waits}"
    );
    assert_eq!(
        pair.wire.calls("orbit.task.pull").len(),
        2,
        "no new request"
    );
    assert_eq!(
        pair.owner_status(&queued_task),
        "backlog",
        "the unlaunched claim is released"
    );
    let released = pair.owner_task(&queued_task);
    assert!(comments_of(&released).contains(&drain), "{released:#}");
    assert!(
        comments_of(&released).contains("host maintenance"),
        "{released:#}"
    );
    assert_eq!(pair.run_state(&queued), JobRunState::Cancelled);
    assert_eq!(
        pair.run_state(&running),
        JobRunState::Running,
        "the launched leaf keeps going"
    );
    assert_eq!(pair.owner_status(&running_task), "in-progress");
    assert_eq!(pair.run_state(&drain), JobRunState::Running);

    pair.leaf_hands_off(&running);
    let done = pair.pass_with(&drain, 2);
    assert_eq!(done["done"], true, "{done}");
    let settles = pair.wire.calls("orbit.drain.claim.settle");
    assert!(
        settles
            .iter()
            .any(|settle| settle["run_id"] == running.as_str()
                && settle["settlement"].get("AcceptHandoff").is_some()),
        "the leaf's own success is delivered: {settles:?}"
    );
    assert_eq!(pair.run_state(&drain), JobRunState::Cancelled);
    assert_eq!(pair.follower.pending_pull_settlements().unwrap().count, 0);
    let state = pair
        .follower
        .read_run_state(&drain)
        .unwrap()
        .expect("state");
    assert!(
        state.drain_cancelling(),
        "the cancel request stays on record"
    );
}

/// A drain owner that cannot be confirmed stopped must keep every carried
/// claim and leave both the drain and its leaves unfinalized.
#[test]
fn a_forced_drain_cancel_refuses_an_unconfirmed_drain_owner() {
    if !isolated(
        module_path!(),
        "a_forced_drain_cancel_refuses_an_unconfirmed_drain_owner",
    ) {
        return;
    }
    let pair = Pair::new(2);
    let drain = pair.run_drain();
    let (running, worker) = pair.running_leaf_with_worker(&drain, 2);
    let queued = pair.queued_leaf(&drain, 2);
    let claims_before = pair.owner_claims();
    let state_before = pair.follower.read_run_state(&drain).unwrap();

    let error = pair
        .follower
        .cancel_job_run_with_options(&drain, "operator", "cli", None, true)
        .expect_err("an unconfirmed drain stop cannot release claims");
    assert!(matches!(error, OrbitError::Execution(_)), "{error}");
    let diagnostic = error.to_string();
    assert!(diagnostic.contains(&drain), "{diagnostic}");
    assert!(diagnostic.contains("stopped"), "{diagnostic}");
    assert!(diagnostic.contains("self_not_signalled"), "{diagnostic}");
    assert_eq!(pair.run_state(&drain), JobRunState::Running);
    assert_eq!(pair.follower.read_run_state(&drain).unwrap(), state_before);
    assert!(worker.running(), "the leaf is never signalled");
    assert_eq!(pair.run_state(&running), JobRunState::Running);
    assert_eq!(pair.run_state(&queued), JobRunState::Pending);
    for leaf in [&running, &queued] {
        assert_eq!(pair.admission(leaf).settlement, None);
        assert_eq!(pair.owner_status(&pair.claimed_task(leaf)), "in-progress");
    }
    assert_eq!(pair.owner_claims(), claims_before);
    assert!(pair.wire.calls("orbit.drain.claim.settle").is_empty());
    assert_eq!(pair.follower.pending_pull_settlements().unwrap().count, 0);
}

/// An owner that already exited still permits forced leaf cancellation and
/// delivery of both launched and unlaunched claim releases.
#[test]
fn a_forced_drain_cancel_accepts_an_already_exited_drain_owner() {
    if !isolated(
        module_path!(),
        "a_forced_drain_cancel_accepts_an_already_exited_drain_owner",
    ) {
        return;
    }
    let pair = Pair::new(2);
    let drain_worker = Worker::spawn();
    let drain = pair.run_drain_with_worker(drain_worker.pid);
    let (running, worker) = pair.running_leaf_with_worker(&drain, 2);
    let queued = pair.queued_leaf(&drain, 2);
    assert!(
        std::process::Command::new("kill")
            .args(["-9", &drain_worker.pid.to_string()])
            .status()
            .unwrap()
            .success()
    );
    assert!(
        drain_worker.stopped(),
        "the owner has exited and been reaped"
    );

    let cancel = pair
        .follower
        .cancel_job_run_with_options(&drain, "operator", "cli", None, true)
        .expect("an already-exited owner permits forced cancellation");
    assert_eq!(cancel.signal_outcome.as_deref(), Some("already_exited"));
    assert_eq!(cancel.outcome, "cancelled");
    assert_eq!(cancel.forced_runs, vec![running.clone()]);
    assert!(cancel.unstopped_leaves.is_empty(), "{cancel:?}");
    assert!(worker.stopped());
    assert_eq!(pair.run_state(&drain), JobRunState::Cancelled);
    for leaf in [&running, &queued] {
        assert_eq!(pair.run_state(leaf), JobRunState::Cancelled);
        assert_eq!(pair.owner_status(&pair.claimed_task(leaf)), "backlog");
        assert_eq!(pair.admission(leaf).phase, LocalPullPhase::Settled);
    }
    let settles = pair.wire.calls("orbit.drain.claim.settle");
    assert_eq!(settles.len(), 2, "{settles:?}");
    assert!(
        settles
            .iter()
            .all(|settle| settle["settlement"].get("Release").is_some())
    );
    assert_eq!(pair.follower.pending_pull_settlements().unwrap().count, 0);
}

/// [ORB-13892] `force` stops a running leaf instead of waiting for it: its
/// claim is released before the leaf is cancelled, and both the running and
/// the unlaunched claim go back to the owner's backlog with a comment naming
/// the drain and the reason.
#[test]
fn a_forced_drain_cancel_stops_running_leaves_and_returns_their_tasks_to_backlog() {
    if !isolated(
        module_path!(),
        "a_forced_drain_cancel_stops_running_leaves_and_returns_their_tasks_to_backlog",
    ) {
        return;
    }
    let pair = Pair::new(2);
    let drain_worker = Worker::spawn();
    let drain = pair.run_drain_with_worker(drain_worker.pid);
    let (running, worker) = pair.running_leaf_with_worker(&drain, 2);
    let queued = pair.queued_leaf(&drain, 2);

    let cancel = pair
        .follower
        .cancel_job_run_with_options(&drain, "operator", "cli", Some("host maintenance"), true)
        .expect("forced cancel");
    assert_eq!(cancel.outcome, "cancelled");
    assert!(
        drain_worker.stopped(),
        "the drain worker stops before release"
    );
    assert!(
        matches!(
            cancel.signal_outcome.as_deref(),
            Some(
                "terminated_process_group"
                    | "killed_process_group"
                    | "terminated_owner"
                    | "killed_owner"
            )
        ),
        "{cancel:?}"
    );
    assert_eq!(cancel.forced_runs, vec![running.clone()]);
    assert!(cancel.unstopped_leaves.is_empty(), "{cancel:?}");
    assert!(worker.stopped(), "the leaf's worker is stopped");
    assert_eq!(pair.run_state(&drain), JobRunState::Cancelled);
    for leaf in [&running, &queued] {
        assert_eq!(pair.run_state(leaf), JobRunState::Cancelled, "{leaf}");
        let task = pair.claimed_task(leaf);
        let owner = pair.owner_task(&task);
        assert_eq!(owner["status"], "backlog", "{owner:#}");
        assert!(comments_of(&owner).contains(&drain), "{owner:#}");
        assert!(
            comments_of(&owner).contains("host maintenance"),
            "{owner:#}"
        );
    }
    let settles = pair.wire.calls("orbit.drain.claim.settle");
    assert!(
        settles
            .iter()
            .all(|settle| settle["settlement"].get("Release").is_some()),
        "nothing is failed: {settles:?}"
    );
    assert_eq!(pair.follower.pending_pull_settlements().unwrap().count, 0);
}

/// [ORB-13892] `force` stops only what the cancelled drain carries: a leaf
/// another live drain for the same owner admitted keeps running, and its
/// task stays claimed.
#[test]
fn a_forced_drain_cancel_leaves_another_live_drains_leaves_alone() {
    if !isolated(
        module_path!(),
        "a_forced_drain_cancel_leaves_another_live_drains_leaves_alone",
    ) {
        return;
    }
    let pair = Pair::new(2);
    let drain_worker = Worker::spawn();
    let cancelled = pair.run_owner_drain(drain_worker.pid);
    let other = pair.run_owner_drain(std::process::id());
    let (mine, my_worker) = pair.running_leaf_with_worker(&cancelled, 2);
    let (theirs, their_worker) = pair.running_leaf_with_worker(&other, 2);

    let cancel = pair
        .follower
        .cancel_job_run_with_options(&cancelled, "operator", "cli", None, true)
        .expect("forced cancel");
    assert_eq!(cancel.forced_runs, vec![mine.clone()]);
    assert!(my_worker.stopped());
    assert_eq!(pair.owner_status(&pair.claimed_task(&mine)), "backlog");

    assert!(
        their_worker.running(),
        "another drain's leaf is not signalled"
    );
    assert_eq!(pair.run_state(&theirs), JobRunState::Running);
    assert_eq!(pair.run_state(&other), JobRunState::Running);
    assert_eq!(
        pair.owner_status(&pair.claimed_task(&theirs)),
        "in-progress"
    );
    assert_eq!(pair.admission(&theirs).settlement, None);
}

/// [ORB-13892] A leaf `force` cannot stop and see gone keeps its claim: no
/// release is recorded or sent, the cancel reports it as unstopped, and the
/// leaf's own outcome still reaches the owner when it ends.
#[test]
fn a_forced_drain_cancel_keeps_the_claim_of_a_leaf_it_cannot_confirm_stopped() {
    if !isolated(
        module_path!(),
        "a_forced_drain_cancel_keeps_the_claim_of_a_leaf_it_cannot_confirm_stopped",
    ) {
        return;
    }
    let pair = Pair::new(1);
    let drain_worker = Worker::spawn();
    let drain = pair.run_drain_with_worker(drain_worker.pid);
    // Its worker is this process, which a cancel never signals.
    let leaf = pair.running_leaf(&drain, 1);
    let task = pair.claimed_task(&leaf);

    let cancel = pair
        .follower
        .cancel_job_run_with_options(&drain, "operator", "cli", None, true)
        .expect("forced cancel");
    assert!(cancel.forced_runs.is_empty(), "{cancel:?}");
    assert_eq!(cancel.unstopped_leaves.len(), 1, "{cancel:?}");
    assert_eq!(cancel.unstopped_leaves[0].leaf_run_id, leaf);
    assert!(
        cancel.unstopped_leaves[0].reason.contains("claim stays"),
        "{cancel:?}"
    );
    assert_eq!(pair.run_state(&drain), JobRunState::Cancelled);
    assert_eq!(pair.run_state(&leaf), JobRunState::Running);
    assert_eq!(
        pair.admission(&leaf).settlement,
        None,
        "nothing was released"
    );
    assert_eq!(pair.owner_status(&task), "in-progress");
    assert!(pair.wire.calls("orbit.drain.claim.settle").is_empty());

    pair.leaf_hands_off(&leaf);
    pair.follower.deliver_recorded_pull_settlements();
    let settles = pair.wire.calls("orbit.drain.claim.settle");
    assert_eq!(settles.len(), 1, "{settles:?}");
    assert!(
        settles[0]["settlement"].get("AcceptHandoff").is_some(),
        "{settles:?}"
    );
}

/// [ORB-13892] The existing MCP stop control takes `force`: it stops
/// admissions, cancels the drain, stops its running leaf and returns the
/// leaf's task to the owner's backlog.
#[test]
fn the_mcp_stop_control_with_force_stops_claimed_leaves() {
    if !isolated(
        module_path!(),
        "the_mcp_stop_control_with_force_stops_claimed_leaves",
    ) {
        return;
    }
    let pair = Pair::new(1);
    let drain_worker = Worker::spawn();
    let drain = pair.run_drain_with_worker(drain_worker.pid);
    let (leaf, worker) = pair.running_leaf_with_worker(&drain, 1);
    let task = pair.claimed_task(&leaf);

    let stopped = pair
        .follower
        .run_tool_with_context_and_role(
            "orbit.workflow.auto",
            json!({
                "workspace": pair.follower.workspace_id().unwrap(),
                "action": "stop",
                "force": true,
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
        .expect("forced stop");
    assert_eq!(stopped["outcome"], "force_cancelled", "{stopped:#}");
    assert_eq!(stopped["coordinators"][0]["run_id"], drain.as_str());
    assert_eq!(
        stopped["coordinators"][0]["forced_runs"],
        json!([leaf]),
        "{stopped:#}"
    );
    assert!(worker.stopped());
    assert_eq!(pair.run_state(&drain), JobRunState::Cancelled);
    assert_eq!(pair.run_state(&leaf), JobRunState::Cancelled);
    assert_eq!(pair.owner_status(&task), "backlog");
}

/// A failed detached child does not hide the parent or prevent a later child
/// from stopping. Both an unconfirmed worker and a missing run stay visible.
#[test]
fn a_forced_local_drain_cancel_reports_mixed_child_outcomes() {
    if !isolated(
        module_path!(),
        "a_forced_local_drain_cancel_reports_mixed_child_outcomes",
    ) {
        return;
    }
    let pair = Pair::new(0);
    let jobs = &pair.follower_jobs;
    let drain = jobs
        .insert_job_run("workspace_auto_pipeline", 1, Utc::now(), None, None)
        .unwrap();
    let failed = jobs
        .insert_job_run("task_auto_pipeline", 1, Utc::now(), None, None)
        .unwrap();
    jobs.mark_job_run_running(&failed.run_id, Utc::now(), std::process::id())
        .unwrap();
    let worker = Worker::spawn();
    let stopped = jobs
        .insert_job_run("task_auto_pipeline", 1, Utc::now(), None, None)
        .unwrap();
    jobs.mark_job_run_running(&stopped.run_id, Utc::now(), worker.pid)
        .unwrap();
    let terminal = jobs
        .insert_job_run("task_auto_pipeline", 1, Utc::now(), None, None)
        .unwrap();
    pair.follower.cancel_job_run(&terminal.run_id).unwrap();
    let missing = "jrun-missing-child";
    let mut state = PipelineState::new(drain.run_id.clone(), drain.job_id, json!({}));
    for child in [
        failed.run_id.as_str(),
        missing,
        &stopped.run_id,
        &terminal.run_id,
    ] {
        state.record_child_dispatch(orbit_types::workflow::ChildDispatch::submitted(
            child.into(),
            "task_auto_pipeline".into(),
            "dispatch".into(),
            false,
            false,
            Utc::now(),
        ));
    }
    pair.follower
        .write_run_state(&drain.run_id, &state)
        .unwrap();
    let cancel = pair
        .follower
        .cancel_job_run_with_options(&drain.run_id, "operator", "cli", None, true)
        .unwrap();
    assert_eq!(cancel.outcome, "cancelled");
    assert_eq!(cancel.final_state, "cancelled");
    assert_eq!(cancel.forced_runs, vec![stopped.run_id.clone()]);
    assert!(worker.stopped());
    assert_eq!(pair.run_state(&drain.run_id), JobRunState::Cancelled);
    assert_eq!(pair.run_state(&stopped.run_id), JobRunState::Cancelled);
    assert_eq!(pair.run_state(&failed.run_id), JobRunState::Running);
    assert_eq!(cancel.unstopped_children.len(), 2, "{cancel:?}");
    assert_eq!(cancel.unstopped_children[0].child_run_id, failed.run_id);
    assert!(
        cancel.unstopped_children[0]
            .reason
            .contains("could not confirm"),
        "{cancel:?}"
    );
    assert_eq!(cancel.unstopped_children[1].child_run_id, missing);
    assert!(!cancel.unstopped_children[1].reason.is_empty());

    // The workspace stop control is another consumer of the cancel result:
    // it must fail with the same child identity rather than drop the field.
    let another = jobs
        .insert_job_run("workspace_auto_pipeline", 1, Utc::now(), None, None)
        .unwrap();
    let mut state = PipelineState::new(another.run_id.clone(), another.job_id, json!({}));
    state.record_child_dispatch(orbit_types::workflow::ChildDispatch::submitted(
        failed.run_id.clone(),
        "task_auto_pipeline".into(),
        "dispatch".into(),
        false,
        false,
        Utc::now(),
    ));
    pair.follower
        .write_run_state(&another.run_id, &state)
        .unwrap();
    let error = pair.follower.run_tool_with_context_and_role(
        "orbit.workflow.auto",
        json!({"workspace": pair.follower.workspace_id().unwrap(), "action": "stop", "force": true}),
        Role::Admin,
        ToolContext {
            session_context: ToolSessionContext {
                transport: Some(McpTransport::Local),
                effective_capabilities: BTreeSet::from([McpCapability::Operator]),
                ..ToolSessionContext::default()
            },
            ..ToolContext::default()
        },
    ).expect_err("an unconfirmed detached child makes the forced stop fail");
    assert!(matches!(error, OrbitError::Execution(_)), "{error}");
    assert!(error.to_string().contains(&failed.run_id), "{error}");
    assert!(error.to_string().contains("could not confirm"), "{error}");
    assert_eq!(pair.run_state(&another.run_id), JobRunState::Cancelled);
    assert_eq!(pair.run_state(&failed.run_id), JobRunState::Running);
}
