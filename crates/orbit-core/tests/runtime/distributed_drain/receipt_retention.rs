//! Admission receipt maintenance through the owner's public runtime API.

use orbit_core::runtime::task_pr_forge::{ForgePullRequest, MergedPullRequest, TaskPrForge};
use orbit_store::contracts::{ClaimEvidence, ClaimInvocation, ClaimMutation};

use super::*;

#[derive(Default)]
struct RetentionForge(Mutex<Option<MergedPullRequest>>);

impl TaskPrForge for RetentionForge {
    fn open_pull_requests(&self, _: &Path) -> Result<Vec<ForgePullRequest>, OrbitError> {
        Ok(Vec::new())
    }

    fn merged_pull_requests(&self, _: &Path) -> Result<Vec<MergedPullRequest>, OrbitError> {
        Ok(self.0.lock().unwrap().iter().cloned().collect())
    }

    fn authenticated_login(&self, _: &Path) -> Result<String, OrbitError> {
        Ok("owner-bot".into())
    }

    fn close_pull_request(&self, _: &Path, _: u64, _: &str) -> Result<(), OrbitError> {
        panic!("receipt retention and external settlement must not close a PR")
    }
}

/// A refused handoff's failed claim can be the only authority for its kept
/// PR. Maintenance must preserve that authority for a later external merge.
#[test]
fn failed_claims_kept_unaccepted_pr_settles_after_receipt_compaction() {
    if !isolated(
        module_path!(),
        "failed_claims_kept_unaccepted_pr_settles_after_receipt_compaction",
    ) {
        return;
    }
    let forge = Arc::new(RetentionForge::default());
    let pair = Pair::with_owner(1, |owner| owner.with_task_pr_forge(forge.clone()));
    let owner = &pair.wire.owner;
    let probe = pair.wire.call("", "orbit.drain.probe", json!({})).unwrap();
    let admitted = pair
        .wire
        .call(
            "",
            "orbit.task.pull",
            json!({
                "request_id": "kept-pr-retention", "caller_version": probe["binary_version"],
                "caller_schema": orbit_store::contracts::DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA,
                "caller_fingerprint": orbit_store::contracts::distributed_drain_protocol_fingerprint(),
                "review_gate": true, "ship": probe["ship"],
                "run_context": {"run_id": "retention-drain", "job_name": "workspace_pull_pipeline"}
            }),
        )
        .unwrap();
    let claim = &admitted["receipt"]["claim"];
    let task = claim["task_id"].as_str().unwrap();
    let claim_id = claim["claim_id"].as_str().unwrap();
    let head = "0123456789abcdef0123456789abcdef01234567";
    let branch = format!("orbit/{task}-01234567");
    owner
        .mutate_execution_claim(
            Some(&ClaimInvocation::trusted_worker(
                task.into(),
                claim_id.into(),
                FOLLOWER.into(),
                None,
            )),
            "fail-with-kept-pr",
            &ClaimMutation::Fail(ClaimEvidence {
                summary: Some("The leaf failed after publishing its candidate PR.".into()),
                failure: Some(
                    serde_json::from_value(json!({
                        "class": "candidate", "reason": "The owner refused the handoff",
                        "candidate": {
                            "branch": branch, "head_sha": head,
                            "pull_request": "42", "published": true
                        }
                    }))
                    .unwrap(),
                ),
                ..Default::default()
            }),
        )
        .unwrap();
    assert_eq!(pair.owner_status(task), "blocked");
    assert!(owner.accepted_task_handoff(claim_id).is_err());
    assert!(owner.get_task(task).unwrap().external_refs.is_empty());
    assert_eq!(
        pair.owner_claim(claim_id)["preserved_candidate"]["candidate"]["pull_request"],
        "42"
    );

    let compacted = owner.compact_admission_receipts().unwrap();
    *forge.0.lock().unwrap() = Some(MergedPullRequest {
        pull: ForgePullRequest {
            number: 42,
            head_branch: branch,
            body: format!("Delivers {task}"),
            // The follower's PR is authoritative even though it has another author.
            author: "follower-bot".into(),
        },
        base_branch: probe["ship"]["landing_branch"].as_str().unwrap().into(),
        head_sha: head.into(),
        merge_commit: head.into(),
    });
    let settlement = owner
        .settle_externally_merged_task(task)
        .expect("the failed claim's kept PR still authorizes settlement after compaction");
    assert_eq!(settlement.pr_number, 42);
    assert_eq!(pair.owner_status(task), "done");
    assert_eq!(compacted.compacted_receipts, 0);
    assert_eq!(compacted.storage.full_receipts.count, 1);
    assert_eq!(compacted.storage.tombstones.count, 0);
}

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
