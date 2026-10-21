//! Refused settlements cannot create ledgers or replace operator reset history.

use super::*;
use orbit_store::contracts::{ClaimAttribution, ClaimedReviewCarry, ReviewResetRequest};
use orbit_types::workflow::{ReviewBudget, ReviewLedger};

fn session(machine: &str) -> ToolSessionContext {
    ToolSessionContext {
        caller_machine_id: Some(machine.into()),
        process_machine_id: Some(OWNER.into()),
        transport: Some(McpTransport::SshMcp),
        effective_capabilities: BTreeSet::from([McpCapability::Agent]),
        ..ToolSessionContext::default()
    }
}

fn evidence(pair: &Pair, leaf: &str) -> ClaimEvidence {
    let claim = pair.admission(leaf).receipt.unwrap().claim.unwrap();
    let ledger = ReviewLedger::new(
        &format!("follower/{}/main/{}", claim.task_id, claim.claim_id),
        vec![claim.task_id],
        ReviewBudget { minutes: 30 },
        Utc::now(),
    );
    ClaimEvidence {
        summary: Some("executor returned this attempt".into()),
        claimed_review: Some(ClaimedReviewCarry {
            executor_machine_id: FOLLOWER.into(),
            executor_data_root: pair.follower.global_root().display().to_string(),
            executor_workspace_id: pair.follower.workspace_id().unwrap(),
            ledgers: vec![ledger],
        }),
        ..Default::default()
    }
}

fn mutation(fail: bool, evidence: ClaimEvidence) -> ClaimMutation {
    if fail {
        ClaimMutation::Fail(evidence)
    } else {
        ClaimMutation::Release(evidence)
    }
}

fn reset(pair: &Pair, ledger: &ReviewLedger) -> ReviewLedger {
    pair.wire
        .owner
        .review_store()
        .unwrap()
        .review_reset(
            &pair.wire.owner.workspace_id().unwrap(),
            &ReviewResetRequest {
                lineage_key: &ledger.lineage_key,
                task_id: &ledger.task_ids[0],
                reason: "operator authorized another attempt",
                actor: "operator",
                budget: None,
                now: Utc::now(),
            },
        )
        .unwrap()
}

fn read(pair: &Pair, key: &str) -> Option<ReviewLedger> {
    pair.wire
        .owner
        .review_store()
        .unwrap()
        .review_ledger(&pair.wire.owner.workspace_id().unwrap(), key)
        .unwrap()
}

// Each refused payload carries both a missing ledger and an existing reset
// ledger with more incoming decisions, exercising both importer write paths.
fn refused_settlements_leave_review_ledgers_unchanged() {
    for fail in [false, true] {
        for refusal in ["machine", "run", "revoked", "replay"] {
            let pair = Pair::new(1);
            let drain = pair.run_drain();
            let leaf = pair.running_leaf(&drain, 1);
            let claim = pair.admission(&leaf).receipt.unwrap().claim.unwrap();
            let original = evidence(&pair, &leaf);
            let ledger = &original.claimed_review.as_ref().unwrap().ledgers[0];
            pair.wire
                .owner
                .review_store()
                .unwrap()
                .review_import_claimed_ledger(&pair.wire.owner.workspace_id().unwrap(), ledger)
                .unwrap();
            let before = reset(&pair, ledger);
            if refusal == "replay" {
                pair.wire
                    .owner
                    .serve_claim_settle(
                        &session(FOLLOWER),
                        &claim.claim_id,
                        Some(&leaf),
                        mutation(fail, original.clone()),
                        ClaimAttribution::default(),
                    )
                    .unwrap();
            } else if refusal == "revoked" {
                pair.wire
                    .owner
                    .recover_claim_as_operator(
                        &claim.claim_id,
                        "running",
                        orbit_types::task::TaskStatus::Backlog,
                        "operator",
                        "operator revoked attempt",
                        "operator-recover",
                    )
                    .unwrap();
            }
            let mut incoming = original.clone();
            let carry = incoming.claimed_review.as_mut().unwrap();
            let mut replacement = before.clone();
            replacement.decisions.push(before.decisions[0].clone());
            let mut missing = replacement.clone();
            missing.lineage_key = format!("follower/{}/other/{}", claim.task_id, claim.claim_id);
            carry.ledgers = vec![missing.clone(), replacement];
            let claims_before = pair.owner_claims();
            let task_before = pair.owner_task(&claim.task_id);
            let error = pair
                .wire
                .owner
                .serve_claim_settle(
                    &session(if refusal == "machine" {
                        "hm_other"
                    } else {
                        FOLLOWER
                    }),
                    &claim.claim_id,
                    Some(if refusal == "run" { "wrong-run" } else { &leaf }),
                    mutation(fail, incoming),
                    ClaimAttribution::default(),
                )
                .expect_err("unauthorized or changed settlement must be refused");
            if refusal == "replay" {
                assert!(
                    matches!(&error, OrbitError::InvalidInput(s) if s == "mutation_mismatch"),
                    "{error}"
                );
            } else {
                assert_eq!(
                    error.claim_refusal(),
                    Some(orbit_common::ClaimRefusalKind::StaleClaim),
                    "{error}"
                );
            }
            assert_eq!(
                read(&pair, &missing.lineage_key),
                None,
                "{refusal}, fail={fail}: refused settlement created a ledger"
            );
            assert_eq!(
                read(&pair, &before.lineage_key),
                Some(before),
                "{refusal}, fail={fail}: refused settlement replaced reset history"
            );
            assert_eq!(pair.owner_claims(), claims_before);
            assert_eq!(pair.owner_task(&claim.task_id), task_before);
        }
    }
}

fn settlement_review_scope_and_replay_preserve_operator_reset() {
    for fail in [false, true] {
        let pair = Pair::new(1);
        let drain = pair.run_drain();
        let leaf = pair.running_leaf(&drain, 1);
        let claim = pair.admission(&leaf).receipt.unwrap().claim.unwrap();
        let original = evidence(&pair, &leaf);
        let ledger = &original.claimed_review.as_ref().unwrap().ledgers[0];
        let task_before = pair.owner_task(&claim.task_id);
        let claims_before = pair.owner_claims();
        for foreign in ["claim", "task", "extra_task", "executor"] {
            let mut incoming = original.clone();
            let carry = incoming.claimed_review.as_mut().unwrap();
            let mut unrelated = ledger.clone();
            match foreign {
                "claim" => unrelated.lineage_key = "follower/task/main/unrelated-claim".into(),
                "task" => unrelated.task_ids = vec!["unrelated-task".into()],
                "extra_task" => unrelated.task_ids.push("unrelated-task".into()),
                _ => carry.executor_machine_id = "hm_other".into(),
            }
            carry.ledgers.push(unrelated.clone());
            pair.wire
                .owner
                .serve_claim_settle(
                    &session(FOLLOWER),
                    &claim.claim_id,
                    Some(&leaf),
                    mutation(fail, incoming),
                    ClaimAttribution::default(),
                )
                .expect_err("a foreign carry must be refused before any mutation");
            assert_eq!(read(&pair, &ledger.lineage_key), None);
            assert_eq!(read(&pair, &unrelated.lineage_key), None);
            assert_eq!(pair.owner_task(&claim.task_id), task_before);
            assert_eq!(pair.owner_claims(), claims_before);
        }
        // Exercise the registered MCP/tool adapter for accepted delivery and replay.
        let input = json!({"claim_id": claim.claim_id, "run_id": leaf, "settlement": mutation(fail, original.clone())});
        // Model an interruption after the durable decision but before import.
        let auth = ClaimInvocation::trusted_worker(
            claim.task_id.clone(),
            claim.claim_id.clone(),
            FOLLOWER.into(),
            Some(ClaimRun {
                machine_id: FOLLOWER.into(),
                run_id: leaf.clone(),
            }),
        );
        let mutation_id = format!(
            "pull-{}:{}",
            if fail { "fail" } else { "release" },
            claim.claim_id
        );
        pair.wire
            .owner
            .mutate_execution_claim(Some(&auth), &mutation_id, &mutation(fail, original.clone()))
            .unwrap();
        assert_eq!(read(&pair, &ledger.lineage_key), None);
        pair.wire
            .call("", "orbit.drain.claim.settle", input.clone())
            .unwrap();
        assert!(read(&pair, &ledger.lineage_key).is_some());
        let after_reset = reset(&pair, ledger);
        pair.wire
            .call("", "orbit.drain.claim.settle", input)
            .unwrap();
        assert_eq!(
            read(&pair, &ledger.lineage_key),
            Some(after_reset),
            "exact replay must preserve an operator reset"
        );
    }
}

crate::scenario_table!(
    settlement_review_scenarios,
    refused_settlements_leave_review_ledgers_unchanged,
    settlement_review_scope_and_replay_preserve_operator_reset
);
