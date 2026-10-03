//! The owner's mutating surface [ORB-13625]: pull, bind and settle, driven
//! through the tool boundary a follower's federated session reaches.

use super::*;

const OTHER_FOLLOWER: &str = "hm_other_follower";

/// A second follower, holding the same `agent` capability. It can reach the
/// owner exactly as the first can; what it cannot do is act on a claim the
/// owner admitted to the first.
fn other_follower_session() -> ToolSessionContext {
    ToolSessionContext {
        caller_machine_id: Some(OTHER_FOLLOWER.to_string()),
        ..follower_session()
    }
}

/// A pull request the way the follower drain builds it: its own identity, and
/// the ship contract the owner's probe reported.
fn pull_input(runtime: &OrbitRuntime, request_id: &str) -> Value {
    let probe = run_as(runtime, follower_session(), "orbit.drain.probe", json!({})).expect("probe");
    json!({
        "request_id": request_id,
        "caller_version": owner_binary_version(),
        "caller_schema": 1,
        "caller_review_policy": "none",
        "run_context": {"run_id": "pull-drain-1", "job_name": "workspace_pull_pipeline"},
        "ship": probe["ship"],
    })
}

fn pulled_claim(runtime: &OrbitRuntime, request_id: &str) -> (Value, String) {
    let response = run_as(
        runtime,
        follower_session(),
        "orbit.task.pull",
        pull_input(runtime, request_id),
    )
    .expect("pull admits");
    let claim_id = response["receipt"]["claim"]["claim_id"]
        .as_str()
        .expect("claim id")
        .to_string();
    (response, claim_id)
}

#[test]
fn a_follower_pull_claims_one_task_for_its_own_machine_and_replays_the_receipt() {
    if !enter_isolated_child(
        "serve::a_follower_pull_claims_one_task_for_its_own_machine_and_replays_the_receipt",
    ) {
        return;
    }
    let (_root, runtime, repo_root) = test_runtime();
    let task = create_context_task(&runtime, &repo_root, TaskStatus::Backlog, &["src/a.rs"]);

    let (response, _claim_id) = pulled_claim(&runtime, "req-1");
    let claim = &response["receipt"]["claim"];
    assert_eq!(claim["task_id"], task.id.as_str());
    // The claim is fenced on the session's machine; nothing in the input
    // named one.
    assert_eq!(claim["executed_on"]["machine_id"], FOLLOWER);
    assert_eq!(response["claim_state"], "claimed");
    assert_eq!(
        runtime.get_task(&task.id).expect("task").status,
        TaskStatus::InProgress
    );

    // A lost answer retried unchanged replays the same admission.
    let replay = run_as(
        &runtime,
        follower_session(),
        "orbit.task.pull",
        pull_input(&runtime, "req-1"),
    )
    .expect("replay");
    assert_eq!(replay["receipt"], response["receipt"]);

    // The same ID with different input is a caller bug, never a second claim.
    let mut changed = pull_input(&runtime, "req-1");
    changed["run_context"]["run_id"] = json!("another-drain");
    let mismatch = run_as(&runtime, follower_session(), "orbit.task.pull", changed)
        .expect_err("changed input refused");
    assert!(
        mismatch.to_string().contains("request_mismatch"),
        "{mismatch}"
    );

    // A fresh request with nothing else ready is idle: a receipt, no claim.
    let idle = run_as(
        &runtime,
        follower_session(),
        "orbit.task.pull",
        pull_input(&runtime, "req-2"),
    )
    .expect("idle is success");
    assert!(idle["receipt"]["claim"].is_null(), "{idle}");
    assert!(idle["claim_state"].is_null());
}

#[test]
fn a_new_request_must_carry_the_ship_contract_the_owner_resolves_now() {
    if !enter_isolated_child(
        "serve::a_new_request_must_carry_the_ship_contract_the_owner_resolves_now",
    ) {
        return;
    }
    let (_root, runtime, repo_root) = test_runtime();
    let task = create_context_task(&runtime, &repo_root, TaskStatus::Backlog, &["src/a.rs"]);

    let mut stale = pull_input(&runtime, "req-1");
    stale["ship"]["base_branch"] = json!("some-old-base");
    let error = run_as(&runtime, follower_session(), "orbit.task.pull", stale)
        .expect_err("stale contract refused");
    assert!(
        error.to_string().contains("ship_contract_mismatch"),
        "{error}"
    );
    assert!(
        runtime
            .inspect_distributed_claims()
            .expect("claims")
            .is_empty()
    );
    assert_eq!(
        runtime.get_task(&task.id).expect("task").status,
        TaskStatus::Backlog
    );
}

#[test]
fn bind_and_failure_settlement_are_fenced_to_the_admitted_machine_and_run() {
    if !enter_isolated_child(
        "serve::bind_and_failure_settlement_are_fenced_to_the_admitted_machine_and_run",
    ) {
        return;
    }
    let (_root, runtime, repo_root) = test_runtime();
    let task = create_context_task(&runtime, &repo_root, TaskStatus::Backlog, &["src/a.rs"]);
    let (response, claim_id) = pulled_claim(&runtime, "req-1");
    let ship = response["receipt"]["request"]["ship"].clone();

    // Another machine can reach the owner but cannot start this attempt.
    let foreign = run_as(
        &runtime,
        other_follower_session(),
        "orbit.drain.claim.bind",
        json!({"claim_id": claim_id, "run_id": "leaf-1", "ship": ship}),
    )
    .expect_err("foreign machine refused");
    assert!(!foreign.to_string().is_empty());

    let bound = run_as(
        &runtime,
        follower_session(),
        "orbit.drain.claim.bind",
        json!({"claim_id": claim_id, "run_id": "leaf-1", "ship": ship}),
    )
    .expect("bind");
    assert_eq!(bound["phase"], "running");
    // A lost bind answer retried is the same bind.
    run_as(
        &runtime,
        follower_session(),
        "orbit.drain.claim.bind",
        json!({"claim_id": claim_id, "run_id": "leaf-1", "ship": ship}),
    )
    .expect("bind replay");
    // A second leaf never takes over the claim.
    run_as(
        &runtime,
        follower_session(),
        "orbit.drain.claim.bind",
        json!({"claim_id": claim_id, "run_id": "leaf-2", "ship": ship}),
    )
    .expect_err("a different run is refused");

    let failure = json!({"Fail": {"summary": "leaf failed", "comment": null, "artifacts": []}});
    run_as(
        &runtime,
        other_follower_session(),
        "orbit.drain.claim.settle",
        json!({"claim_id": claim_id, "run_id": "leaf-1", "settlement": failure}),
    )
    .expect_err("foreign settlement refused");
    let settled = run_as(
        &runtime,
        follower_session(),
        "orbit.drain.claim.settle",
        json!({"claim_id": claim_id, "run_id": "leaf-1", "settlement": failure}),
    )
    .expect("settle failure");
    assert_eq!(settled["phase"], "failed");
    assert_eq!(
        runtime.get_task(&task.id).expect("task").status,
        TaskStatus::Blocked
    );
    run_as(
        &runtime,
        follower_session(),
        "orbit.drain.claim.settle",
        json!({"claim_id": claim_id, "run_id": "leaf-1", "settlement": failure}),
    )
    .expect("settlement replay returns the recorded outcome");

    let unknown = run_as(
        &runtime,
        follower_session(),
        "orbit.drain.claim.bind",
        json!({"claim_id": "no-such-claim", "run_id": "leaf-1", "ship": ship}),
    )
    .expect_err("unknown claim");
    assert!(unknown.to_string().contains("stale_claim"), "{unknown}");
}

#[test]
fn settlement_accepts_only_a_handoff_or_a_failure() {
    if !enter_isolated_child("serve::settlement_accepts_only_a_handoff_or_a_failure") {
        return;
    }
    let (_root, runtime, repo_root) = test_runtime();
    create_context_task(&runtime, &repo_root, TaskStatus::Backlog, &["src/a.rs"]);
    let (_response, claim_id) = pulled_claim(&runtime, "req-1");

    let recover = json!({"Recover": {"status": "backlog", "reason": "self-serve"}});
    let error = run_as(
        &runtime,
        follower_session(),
        "orbit.drain.claim.settle",
        json!({"claim_id": claim_id, "settlement": recover}),
    )
    .expect_err("recovery is an owner-operator action");
    assert!(error.to_string().contains("owner-operator"), "{error}");
}

#[test]
fn a_follower_cannot_hand_off_a_candidate_only_its_own_checkout_holds() {
    if !enter_isolated_child(
        "serve::a_follower_cannot_hand_off_a_candidate_only_its_own_checkout_holds",
    ) {
        return;
    }
    use orbit_types::workflow::handoff::{
        HandoffCandidate, HandoffDelivery, HandoffReview, HandoffReviewDisposition, TaskHandoff,
    };
    use orbit_types::workflow::{ReviewTiming, automation::SourceRevision};

    let (_root, runtime, repo_root) = test_runtime();
    let task = create_context_task(&runtime, &repo_root, TaskStatus::Backlog, &["src/a.rs"]);
    let (response, claim_id) = pulled_claim(&runtime, "req-1");
    let ship = response["receipt"]["request"]["ship"].clone();
    run_as(
        &runtime,
        follower_session(),
        "orbit.drain.claim.bind",
        json!({"claim_id": claim_id, "run_id": "leaf-1", "ship": ship}),
    )
    .expect("bind");
    let handoff = TaskHandoff {
        schema_version: 1,
        workspace_id: runtime.workspace_id().expect("workspace"),
        task_id: task.id.clone(),
        claim_id: claim_id.clone(),
        machine_id: FOLLOWER.into(),
        run_id: "leaf-1".into(),
        candidate: HandoffCandidate {
            repository: "owner/repository".into(),
            source_branch: "attempt/leaf-1".into(),
            base_branch: "agent-main".into(),
            landing_branch: "agent-main".into(),
            candidate: SourceRevision {
                commit: "a".repeat(40),
                tree: "b".repeat(40),
            },
            base: SourceRevision {
                commit: "c".repeat(40),
                tree: "d".repeat(40),
            },
            delivery: HandoffDelivery::LocalCandidate,
        },
        review: HandoffReview {
            policy: ReviewTiming::None,
            disposition: HandoffReviewDisposition::NotRequired,
        },
        execution_summary: "Outcome: success".into(),
        validation: Vec::new(),
    };
    let error = run_as(
        &runtime,
        follower_session(),
        "orbit.drain.claim.settle",
        json!({
            "claim_id": claim_id,
            "run_id": "leaf-1",
            "settlement": {"AcceptHandoff": handoff},
        }),
    )
    .expect_err("a remote local candidate is refused");
    assert!(error.to_string().contains("local candidate"), "{error}");
    assert_eq!(
        runtime.get_task(&task.id).expect("task").status,
        TaskStatus::InProgress
    );
}

#[test]
fn a_replica_serves_no_mutating_entry_point() {
    if !enter_isolated_child("serve::a_replica_serves_no_mutating_entry_point") {
        return;
    }
    let (_root, runtime, _repo_root) = test_runtime();
    let input = pull_input(&runtime, "req-1");
    let replica = runtime
        .clone()
        .with_coordination_write_owner(Some(OWNER.to_string()));
    for (tool, input) in [
        ("orbit.task.pull", input),
        (
            "orbit.drain.claim.bind",
            json!({"claim_id": "c", "run_id": "r", "ship": {}}),
        ),
        (
            "orbit.drain.claim.settle",
            json!({"claim_id": "c", "settlement": {"Fail": {"summary": null, "comment": null, "artifacts": []}}}),
        ),
    ] {
        let error = run_as(&replica, follower_session(), tool, input)
            .expect_err("a replica mints no claim");
        assert!(
            matches!(
                error,
                orbit_common::OrbitError::CapabilityRefused(_)
                    | orbit_common::OrbitError::InvalidInput(_)
            ),
            "{tool}: {error}"
        );
    }
    let error = run_as(
        &replica,
        follower_session(),
        "orbit.task.pull",
        pull_input(&runtime, "req-2"),
    )
    .expect_err("replica pull");
    assert!(
        matches!(error, orbit_common::OrbitError::CapabilityRefused(_)),
        "{error}"
    );
}

#[test]
fn a_local_ship_owner_refuses_a_remote_pull_before_admitting() {
    if !enter_isolated_child("serve::a_local_ship_owner_refuses_a_remote_pull_before_admitting") {
        return;
    }
    let (_root, runtime, repo_root) = local_ship_runtime();
    let task = create_context_task(&runtime, &repo_root, TaskStatus::Backlog, &["src/a.rs"]);
    let error = run_as(
        &runtime,
        follower_session(),
        "orbit.task.pull",
        pull_input(&runtime, "req-1"),
    )
    .expect_err("followers never execute owner-local work");
    assert!(
        error.to_string().contains("ship_mode_unsupported"),
        "{error}"
    );
    assert_eq!(
        runtime.get_task(&task.id).expect("task").status,
        TaskStatus::Backlog
    );
}

#[test]
fn a_pull_requires_agent_capability_on_the_session() {
    if !enter_isolated_child("serve::a_pull_requires_agent_capability_on_the_session") {
        return;
    }
    let (_root, runtime, repo_root) = test_runtime();
    create_context_task(&runtime, &repo_root, TaskStatus::Backlog, &["src/a.rs"]);
    let input = pull_input(&runtime, "req-1");
    let anonymous = ToolSessionContext {
        effective_capabilities: BTreeSet::new(),
        ..follower_session()
    };
    run_as(&runtime, anonymous, "orbit.task.pull", input).expect_err("unidentified caller");
    assert!(
        runtime
            .inspect_distributed_claims()
            .expect("claims")
            .is_empty()
    );
}

/// The owner's `workflow.distributed_completion` is what its probe reports and
/// what admission pins: `done` names the owner policy as the authorization
/// reference, and a follower still carrying a `review` contract re-probes
/// before a new request is admitted.
#[test]
fn the_owner_completion_policy_reaches_the_probe_and_admission() {
    if !enter_isolated_child("serve::the_owner_completion_policy_reaches_the_probe_and_admission") {
        return;
    }
    let (_root, runtime, repo_root) = test_runtime();
    let stale = pull_input(&runtime, "stale-review");
    assert_eq!(stale["ship"]["completion"], "review");
    assert!(stale["ship"]["authorization_reference"].is_null());

    std::fs::write(
        repo_root.join(".orbit/config.toml"),
        "[workflow]\ndistributed_completion = \"done\"\n",
    )
    .expect("owner config");
    let runtime = OrbitRuntime::from_roots(&runtime.global_root(), &repo_root.join(".orbit"))
        .expect("reopen with owner policy");
    create_context_task(&runtime, &repo_root, TaskStatus::Backlog, &["src/a.rs"]);

    let probe =
        run_as(&runtime, follower_session(), "orbit.drain.probe", json!({})).expect("probe");
    assert_eq!(probe["ship"]["completion"], "done");
    assert_eq!(
        probe["ship"]["authorization_reference"],
        crate::application::distributed::OWNER_COMPLETION_POLICY
    );

    let refused = run_as(&runtime, follower_session(), "orbit.task.pull", stale)
        .expect_err("a review contract no longer matches this owner");
    assert!(
        refused.to_string().contains("ship_contract_mismatch"),
        "{refused}"
    );
    let (response, _claim_id) = pulled_claim(&runtime, "fresh-done");
    assert_eq!(response["receipt"]["request"]["ship"]["completion"], "done");
}
