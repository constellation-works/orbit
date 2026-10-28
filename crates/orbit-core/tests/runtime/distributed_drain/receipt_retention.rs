//! Admission receipt maintenance through the owner's public runtime API.

use orbit_store::contracts::{ClaimEvidence, ClaimInvocation, ClaimMutation};

use super::*;

#[test]
fn operational_receipt_compaction_preserves_request_identity_and_live_claims() {
    if !isolated(
        module_path!(),
        "operational_receipt_compaction_preserves_request_identity_and_live_claims",
    ) {
        return;
    }
    let pair = Pair::new(2);
    let owner = &pair.wire.owner;
    let probe = pair.wire.call("", "orbit.drain.probe", json!({})).unwrap();
    let request = |id: &str| {
        json!({
            "request_id": id, "caller_version": probe["binary_version"],
            "caller_schema": orbit_store::contracts::DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA,
            "caller_fingerprint": orbit_store::contracts::distributed_drain_protocol_fingerprint(),
            "review_gate": true, "ship": probe["ship"],
            "run_context": {"run_id": "retention-drain", "job_name": "workspace_pull_pipeline"}
        })
    };
    let pull = |input: Value| pair.wire.call("", "orbit.task.pull", input);
    let settled_request = request("settled");
    let live_request = request("live");
    let idle_request = request("idle");
    let settled = pull(settled_request.clone()).unwrap();
    let live = pull(live_request.clone()).unwrap();
    let idle = pull(idle_request.clone()).unwrap();
    assert!(idle["receipt"]["claim"].is_null());

    let before = owner.inspect_admission_receipt_storage().unwrap();
    assert_eq!(before.full_receipts.count, 3);
    assert_eq!(before.tombstones.count, 0);
    let first = owner.compact_admission_receipts().unwrap();
    assert_eq!(first.compacted_receipts, 1, "only idle is eligible");
    assert_eq!(first.storage.full_receipts.count, 2);
    assert_eq!(first.storage.tombstones.count, 1);
    assert_eq!(pull(live_request.clone()).unwrap(), live);

    let claim = &settled["receipt"]["claim"];
    owner
        .mutate_execution_claim(
            Some(&ClaimInvocation::trusted_worker(
                claim["task_id"].as_str().unwrap().into(),
                claim["claim_id"].as_str().unwrap().into(),
                FOLLOWER.into(),
                None,
            )),
            "settle-for-retention",
            &ClaimMutation::Fail(ClaimEvidence {
                summary: Some("Fixture failure ends this claim.".into()),
                ..Default::default()
            }),
        )
        .unwrap();
    let mutations = owner
        .inspect_admission_receipt_storage()
        .unwrap()
        .claim_mutation_receipts;
    assert_eq!(mutations.count, 1);
    assert!(mutations.logical_payload_bytes > 0);
    let second = owner.compact_admission_receipts().unwrap();
    assert_eq!(second.compacted_receipts, 1);
    assert_eq!(second.storage.full_receipts.count, 1);
    assert_eq!(second.storage.tombstones.count, 2);
    assert!(second.storage.tombstones.logical_payload_bytes > 0);
    assert!(
        second.storage.full_receipts.logical_payload_bytes
            + second.storage.tombstones.logical_payload_bytes
            < before.full_receipts.logical_payload_bytes
    );
    assert_eq!(
        second.storage.claim_mutation_receipts, mutations,
        "admission maintenance never prunes mutation receipts"
    );

    for original in [idle_request, settled_request] {
        let error = pull(original.clone()).unwrap_err();
        assert!(error.to_string().contains("request_expired"), "{error}");
        let mut changed = original.clone();
        changed["run_context"]["run_id"] = json!("different-input");
        let error = pull(changed).unwrap_err();
        assert!(error.to_string().contains("request_mismatch"), "{error}");
        let lookup = pair
            .wire
            .call(
                "",
                "orbit.drain.receipt.lookup",
                json!({
                    "request_id": original["request_id"],
                    "lookup_schema": orbit_store::contracts::ADMISSION_RECEIPT_LOOKUP_SCHEMA,
                }),
            )
            .unwrap();
        assert_eq!(lookup["outcome"], "expired");
        assert_eq!(lookup["permits_replacement_request"], false);
    }
    assert_eq!(
        pair.owner_claims().len(),
        2,
        "compacted replays admit no new claim"
    );
    assert_eq!(pull(live_request).unwrap(), live);
    assert_eq!(
        owner
            .compact_admission_receipts()
            .unwrap()
            .compacted_receipts,
        0
    );
    assert_eq!(
        owner.inspect_admission_receipt_storage().unwrap(),
        second.storage
    );
}

/// A recorded landing completion must replay after its admission receipt has
/// compacted; observing the original NoDiff claim first used to refuse it.
#[test]
fn landed_no_diff_completion_replays_after_admission_receipt_compaction() {
    if !isolated(
        module_path!(),
        "landed_no_diff_completion_replays_after_admission_receipt_compaction",
    ) {
        return;
    }
    let fixture = super::no_diff::CleanLeaf::tagged("done");
    let handoff = fixture.run_clean_pipeline("task_claimed_pr_pipeline");
    fixture.settle_review(&handoff);
    let landed = fixture.land(&handoff);
    assert_eq!(landed["phase"], "landed", "{landed}");
    assert_eq!(landed["evidence"]["delivery"], "no_diff", "{landed}");
    fixture.assert_status("done");
    let owner = &fixture.pair.wire.owner;
    let compacted = owner.compact_admission_receipts().unwrap();
    assert_eq!(compacted.compacted_receipts, 1);
    assert_eq!(compacted.storage.full_receipts.count, 0);
    assert_eq!(compacted.storage.tombstones.count, 1);
    let history = owner.get_task_history(&fixture.task).unwrap();
    let attempts = owner.landing_attempts().unwrap();
    let update = orbit_engine::HandoffLandingUpdate {
        handoff_id: fixture.accepted(&handoff).handoff_id,
        step: orbit_engine::HandoffLandingStep::Complete,
        observed: Some(handoff.candidate.clone()),
        evidence: landed["evidence"].to_string(),
    };
    RuntimeHost::record_handoff_landing(owner, &update)
        .expect("recorded Complete replays without the compacted admission receipt");
    let mut changed = update;
    changed.evidence = json!({"delivery": "changed"}).to_string();
    let error = RuntimeHost::record_handoff_landing(owner, &changed)
        .expect_err("changed completion input must not reuse the receipt");
    assert!(error.to_string().contains("mutation_mismatch"), "{error}");
    fixture.assert_status("done");
    assert_eq!(owner.get_task_history(&fixture.task).unwrap(), history);
    assert_eq!(owner.landing_attempts().unwrap(), attempts);
    assert_eq!(
        owner.inspect_admission_receipt_storage().unwrap(),
        compacted.storage
    );
}
