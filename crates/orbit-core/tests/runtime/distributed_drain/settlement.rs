//! Claim/bind replay and settlement delivery: lost replies, owner refusals and backoff, outages and the clock sweep.

use super::*;
use orbit_core::application::job::JobRunCancelOutcome;

/// The session machine fences bind/settle even when the caller knows the
/// claim and bound run. A refused attempt must leave the owner unchanged.
fn another_session_machine_cannot_bind_or_settle_a_followers_claim() {
    let pair = Pair::new(1);
    let drain = pair.run_drain();
    let leaf = pair.running_leaf(&drain, 1);
    let record = pair.admission(&leaf);
    let claim = record.receipt.as_ref().unwrap().claim.as_ref().unwrap();
    let claims_before = pair.owner_claims();
    let task_before = pair.owner_task(&claim.task_id);
    let wrong_session = ToolSessionContext {
        caller_machine_id: Some("hm_other_follower".into()),
        process_machine_id: Some(OWNER.into()),
        transport: Some(McpTransport::SshMcp),
        effective_capabilities: BTreeSet::from([McpCapability::Agent]),
        ..ToolSessionContext::default()
    };
    let evidence = ClaimEvidence {
        summary: Some("executor returned this attempt".into()),
        ..Default::default()
    };
    for mutation in [
        ClaimMutation::Release(evidence.clone()),
        ClaimMutation::Fail(evidence.clone()),
    ] {
        let error = pair
            .wire
            .owner
            .serve_claim_settle(
                &wrong_session,
                &claim.claim_id,
                Some(&leaf),
                mutation,
                orbit_store::contracts::ClaimAttribution::default(),
            )
            .expect_err("another session machine must not settle this claim");
        assert_eq!(
            error.claim_refusal(),
            Some(orbit_common::ClaimRefusalKind::StaleClaim),
            "the claim ownership fence must refuse the wrong session machine: {error}"
        );
    }
    let error = pair
        .wire
        .owner
        .serve_claim_bind(
            &wrong_session,
            &claim.claim_id,
            &leaf,
            record.request.ship.clone(),
        )
        .expect_err("another session machine must not replay the bound claim");
    assert!(
        matches!(&error, OrbitError::InvalidInput(message) if message == "mutation_mismatch"),
        "bind replay must refuse a change to its recorded session machine: {error}"
    );
    assert_eq!(pair.owner_claims(), claims_before);
    assert_eq!(pair.owner_task(&claim.task_id), task_before);

    let matching_session = ToolSessionContext {
        caller_machine_id: Some(FOLLOWER.into()),
        ..wrong_session
    };
    let settled = pair
        .wire
        .owner
        .serve_claim_settle(
            &matching_session,
            &claim.claim_id,
            Some(&leaf),
            ClaimMutation::Release(evidence),
            orbit_store::contracts::ClaimAttribution::default(),
        )
        .expect("the owning session machine can still settle its attempt");
    assert_eq!(
        settled.phase,
        orbit_store::contracts::ExecutionClaimPhase::Revoked
    );
    assert_eq!(pair.owner_status(&claim.task_id), "backlog");
}

/// A host that cannot spawn a claimed leaf returns its task and stops pulling
/// immediately, rather than blocking several tasks to trip the breaker.
fn a_launch_failure_cancels_the_leaf_releases_the_task_and_suppresses_the_host() {
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
    assert_eq!(
        pair.owner_claim_for_leaf(&leaf)["claim"]["phase"],
        "revoked"
    );
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
fn operator_cancelled_claimed_leaf_returns_to_backlog_with_reason() {
    let cancel_reason = "preserve this candidate for later";
    let pair = Pair::new(1);
    let drain = pair.run_drain();
    let leaf = pair.running_leaf(&drain, 1);
    let task = pair.claimed_task(&leaf);
    let comments_before = pair.owner_task(&task)["comments"].as_array().unwrap().len();

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
            Some(cancel_reason),
            false,
            false,
        )
        .expect("cancel claimed leaf");
    assert_eq!(cancelled.outcome, JobRunCancelOutcome::Cancelled);
    assert_eq!(pair.run_state(&leaf), JobRunState::Cancelled);
    assert_eq!(pair.owner_status(&task), "backlog");
    let settles = pair.wire.calls("orbit.drain.claim.settle");
    assert_eq!(settles.len(), 1, "{settles:?}");
    let failure = &settles[0]["settlement"]["Release"]["failure"];
    assert_eq!(failure["class"], "operator_cancel");
    assert!(
        failure["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains(cancel_reason)),
        "settlement failure should preserve the operator reason: {failure:?}"
    );
    let owner_task = pair.owner_task(&task);
    let comments = owner_task["comments"].as_array().expect("owner comments");
    assert_eq!(comments.len(), comments_before + 1);
    assert!(
        comments.last().unwrap()["message"]
            .as_str()
            .is_some_and(|note| note.contains(cancel_reason)),
        "owner comment should preserve the operator reason: {comments:?}"
    );
    let claim = pair.follower.pull_leaf_claim(&leaf).unwrap().unwrap();
    assert_eq!(claim.settlement_phase, "settled");
}

/// A lost pull reply and then a lost bind reply are both retried under the
/// identity the owner already committed: one request, one claim, one leaf,
/// bound and launched once. A drain that gave up on the unanswered request
/// would pull the owner's second task as well.
fn lost_pull_and_bind_replies_recover_the_same_claim_and_leaf_exactly_once() {
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
fn a_settlement_whose_reply_is_lost_is_redelivered_and_applied_once() {
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

/// A recovered claim must be refused before observing the obsolete PR head:
/// otherwise a forge error hides the revocation and every sweep retries it.
fn a_recovered_claim_closes_its_pending_handoff_before_observing_the_pr() {
    let pair = Pair::new(1);
    let drain = pair.run_drain();
    let leaf = pair.running_leaf(&drain, 1);
    let task = pair.claimed_task(&leaf);
    let claim = pair.owner_claim_for_leaf(&leaf);
    pair.leaf_hands_off(&leaf);
    pair.wire
        .owner
        .recover_claim_as_operator(
            claim["claim"]["claim_id"].as_str().unwrap(),
            "running",
            orbit_types::task::TaskStatus::Blocked,
            "operator",
            "The follower stopped; the PR was rescued by hand",
            "recover-pending-handoff",
        )
        .unwrap();
    // Ended claims reach the real owner rather than the transport's fake PR.
    // This checkout has no published candidate, so observing it would fail.
    let log = pair._root.path().join("obsolete-settlement.log");
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_max_level(tracing::Level::WARN)
        .with_writer(std::sync::Mutex::new(std::fs::File::create(&log).unwrap()))
        .finish();
    let _logging = tracing::subscriber::set_default(subscriber);
    let settled = pair.pass(&drain);
    assert!(settled["error"].is_null(), "{settled}");
    assert_eq!(pair.follower.pending_pull_settlements().unwrap().count, 0);
    let closed = pair.follower.pull_leaf_claim(&leaf).unwrap().unwrap();
    assert_eq!(closed.settlement_phase, "settled");
    assert!(closed.refusal.as_deref().unwrap().contains("already ended"));
    assert_eq!(pair.owner_status(&task), "blocked");
    let calls = pair.wire.calls("orbit.drain.claim.settle").len();
    pair.pass(&drain);
    assert_eq!(
        pair.wire.calls("orbit.drain.claim.settle").len(),
        calls,
        "a closed settlement is not sent again"
    );
    let warnings = std::fs::read_to_string(log).unwrap();
    assert_eq!(
        warnings
            .matches("closing an undeliverable pull settlement")
            .count(),
        1,
        "{warnings}"
    );
}

impl Pair {
    /// Let `leaf`'s settlement backoff elapse, as the clock passing it would.
    pub(super) fn elapse_settlement_backoff(&self, leaf: &str) -> SettlementRefusal {
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
fn a_settlement_the_owner_refuses_while_holding_its_claim_backs_off_until_it_accepts() {
    let pair = Pair::new(2);
    let drain = pair.run_drain();
    let leaf = pair.running_leaf(&drain, 1);
    pair.leaf_hands_off(&leaf);
    *pair.wire.refuse_settle.lock().unwrap() = Some(PROTECTED_PATH_WIDENING.into());
    let pulls = pair.wire.calls("orbit.task.pull").len();

    for _ in 0..5 {
        // The pending settlement occupies only one of two slots. Even the
        // pass that first observes the policy refusal must hold new claims.
        let pass = pair.pass_with(&drain, 2);
        assert_eq!(
            pair.wire.calls("orbit.task.pull").len(),
            pulls,
            "a newly observed policy refusal must hold the free slot: {pass}"
        );
        assert!(
            pass["error"].is_null(),
            "a refusal is not a failed pass: {pass}"
        );
        assert_eq!(pass["admitting"], false, "{pass}");
        assert_eq!(pass["settlement_refused"], 1, "{pass}");
        assert_eq!(pass["settlement_refused_holding"], 1, "{pass}");
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
    assert_eq!(refused[0].code.as_deref(), Some("policy_denied"));
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

/// A newly refused settlement holds fresh admission without preventing the
/// same pass from reconciling the other pending records for that owner.
fn a_new_policy_refusal_holds_new_claims_but_reconciles_other_pending_records() {
    let pair = Pair::new(3);
    let drain = pair.run_drain();
    let first = pair.running_leaf(&drain, 1);
    let second = pair.running_leaf(&drain, 2);
    pair.leaf_hands_off(&first);
    pair.leaf_hands_off(&second);
    *pair.wire.refuse_settle.lock().unwrap() = Some(PROTECTED_PATH_WIDENING.into());
    let pulls = pair.wire.calls("orbit.task.pull").len();

    let pass = pair.pass_with(&drain, 3);
    assert_eq!(
        pair.wire.calls("orbit.task.pull").len(),
        pulls,
        "policy refusals must hold the spare slot: {pass}"
    );
    assert!(pass["error"].is_null(), "{pass}");
    assert_eq!(pass["settlement_refused_holding"], 2, "{pass}");
    assert_eq!(pair.wire.calls("orbit.drain.claim.settle").len(), 2);
    for leaf in [&first, &second] {
        let record = pair.admission(leaf);
        assert_eq!(record.phase, LocalPullPhase::Settling);
        assert_eq!(
            record.settlement_refusal.unwrap().code.as_deref(),
            Some("policy_denied"),
            "both pending records must reconcile in the held pass"
        );
    }
}

/// A delivery-evidence refusal (the PR head moved past the published
/// candidate) backs off its own settlement but does not hold the pass:
/// other work is still claimed, and once the owner ends the claim the
/// refused settlement closes as obsolete.
#[cfg(unix)]
fn a_delivery_refused_settlement_backs_off_closes_and_lets_other_work_proceed() {
    let pair = Pair::new(2);
    let drain = pair.run_drain();
    let leaf = pair.running_leaf(&drain, 2);
    let task = pair.claimed_task(&leaf);
    pair.leaf_hands_off(&leaf);
    let bin = std::path::PathBuf::from(std::env::var_os("HOME").unwrap()).join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let gh = bin.join("gh");
    let admission = pair.admission(&leaf);
    let candidate = handoff(&admission).candidate;
    let status = json!({
        "number": 42, "state": "OPEN", "headRefName": candidate.source_branch,
        "baseRefName": candidate.base_branch, "headRefOid": "moved-by-hand",
        "url": "https://github.com/owner/repository/pull/42"
    });
    std::fs::write(&gh, format!("#!/bin/sh\nprintf '%s' '{}'\n", status)).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    // Use the owner's real observation path; the provider reports a head
    // different from the candidate in the follower's handoff.
    *pair.wire.observe_handoffs.lock().unwrap() = true;
    let pulls = pair.wire.calls("orbit.task.pull").len();

    let pass = pair.pass_with(&drain, 2);
    assert_eq!(pass["settlement_refused"], 1, "{pass}");
    assert_eq!(pass["settlement_refused_holding"], 0, "{pass}");
    let refused = pair
        .admission(&leaf)
        .settlement_refusal
        .clone()
        .expect("refused");
    assert_eq!(
        refused.code.as_deref(),
        Some("delivery_head_changed"),
        "{refused:?}"
    );
    assert!(refused.retry_after > refused.last_refused_at, "{refused:?}");
    assert!(
        pair.wire.calls("orbit.task.pull").len() > pulls,
        "a delivery refusal must not stop other claims: {pass}"
    );
    assert_eq!(pair.leaf_runs().len(), 2, "other work was claimed");

    let handoff_settles = pair
        .wire
        .calls("orbit.drain.claim.settle")
        .iter()
        .filter(|call| call["settlement"].get("AcceptHandoff").is_some())
        .count();
    pair.pass_with(&drain, 2);
    assert_eq!(
        pair.wire
            .calls("orbit.drain.claim.settle")
            .iter()
            .filter(|call| call["settlement"].get("AcceptHandoff").is_some())
            .count(),
        handoff_settles,
        "no redelivery before the backoff elapses"
    );

    let claim = pair.owner_claim_for_leaf(&leaf);
    pair.wire
        .owner
        .recover_claim_as_operator(
            claim["claim"]["claim_id"].as_str().unwrap(),
            "running",
            orbit_types::task::TaskStatus::Blocked,
            "operator",
            "The follower stopped; the PR was rescued by hand",
            "recover-delivery-refused-handoff",
        )
        .unwrap();
    pair.elapse_settlement_backoff(&leaf);
    let closed = pair.pass_with(&drain, 2);
    assert!(
        closed["error"].is_null() || launch_refused(&closed),
        "{closed}"
    );
    assert_eq!(pair.follower.pending_pull_settlements().unwrap().count, 0);
    let claim_view = pair.follower.pull_leaf_claim(&leaf).unwrap().unwrap();
    assert_eq!(claim_view.settlement_phase, "settled");
    assert!(
        claim_view
            .refusal
            .as_deref()
            .unwrap()
            .contains("already ended")
    );
    assert_eq!(pair.owner_status(&task), "blocked");
}

/// [ORB-13979] An operator who fixed the owner need not wait out the
/// backoff: `orbit run auto --stop` delivers a refused settlement at once.
fn an_operator_stop_delivers_a_refused_settlement_without_waiting_for_its_backoff() {
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
fn a_pending_settlement_is_retried_after_an_owner_outage_before_the_drain_ends() {
    let pair = Pair::new(2);
    let drain = pair.run_drain();
    let leaf = pair.running_leaf(&drain, 1);
    let cancel = pair
        .follower
        .cancel_job_run_with_options(&drain, "operator", "cli", None, false)
        .expect("cancel");
    assert_eq!(cancel.outcome, JobRunCancelOutcome::Cancelling);

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
fn a_forced_release_the_owner_missed_is_delivered_by_the_clock_sweep() {
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

// Related boundary scenarios share the isolated table fixture.
crate::scenario_table!(
    another_session_machine_cannot_bind_or_settle_a_followers_claim_scenarios,
    another_session_machine_cannot_bind_or_settle_a_followers_claim,
    a_launch_failure_cancels_the_leaf_releases_the_task_and_suppresses_the_host,
    operator_cancelled_claimed_leaf_returns_to_backlog_with_reason,
    lost_pull_and_bind_replies_recover_the_same_claim_and_leaf_exactly_once,
    a_settlement_whose_reply_is_lost_is_redelivered_and_applied_once,
    a_recovered_claim_closes_its_pending_handoff_before_observing_the_pr,
    a_settlement_the_owner_refuses_while_holding_its_claim_backs_off_until_it_accepts,
    a_new_policy_refusal_holds_new_claims_but_reconciles_other_pending_records,
    an_operator_stop_delivers_a_refused_settlement_without_waiting_for_its_backoff,
    a_pending_settlement_is_retried_after_an_owner_outage_before_the_drain_ends,
    a_forced_release_the_owner_missed_is_delivered_by_the_clock_sweep
);

#[cfg(unix)]
crate::scenario_table!(
    delivery_refused_settlement_scenarios,
    a_delivery_refused_settlement_backs_off_closes_and_lets_other_work_proceed,
);
