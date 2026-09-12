//! Recovery keeps every obligation and never substitutes authorization for
//! evidence [ORB-12295].

use super::evidence::{Host, evaluate, landing, now, revision, setup};
use crate::{
    AutomationError,
    delivery::{self, Evaluation, recovery},
};
use orbit_store::contracts::AutomationStoreBackend;
use orbit_types::workflow::automation::recovery::{
    HistoryMapping, HistoryReplayRecord, RecoveryPreview, RecoveryRequest, refusal,
};
use orbit_types::workflow::automation::*;
use std::sync::atomic::Ordering;

const CONSUMER: &str = "ws/qa";

fn request(adopt: bool, reissue: bool) -> RecoveryRequest {
    RecoveryRequest {
        adopt_settings: adopt,
        reissue_action: reissue,
        replay_history: false,
        reason: "tonight's retuning keeps the same QA contract".into(),
    }
}

fn recovery<'a>(
    trigger: &'a DeliveryTrigger,
    epoch: &'a str,
    request: &'a RecoveryRequest,
) -> recovery::Recovery<'a> {
    recovery::Recovery {
        consumer: CONSUMER,
        epoch,
        trigger,
        repository: "owner/repo",
        host_refusal: None,
        request,
        by: "operator",
        now: now(),
        replay: None,
    }
}

/// Drive a consumer to a settled failed action over two retained landings,
/// exactly as an archived unevidenced task leaves it.
fn stalled() -> (
    std::sync::Arc<dyn AutomationStoreBackend>,
    Host,
    DeliveryTrigger,
    CoverageBatch,
) {
    let (store, host, trigger) = setup();
    host.page(0, 2);
    let batch = evaluate(store.as_ref(), &host, &trigger, true)
        .state
        .unwrap()
        .active
        .unwrap()
        .batch;

    // Spend the frozen retry budget: the worker fails, the automatic retry is
    // admitted after its backoff, and that attempt fails too.
    host.page(2, 2);
    host.failed.store(true, Ordering::SeqCst);
    evaluate(store.as_ref(), &host, &trigger, true);
    delivery::evaluate(
        store.as_ref(),
        &host,
        Evaluation {
            consumer: CONSUMER,
            epoch: "v1",
            trigger: &trigger,
            enabled: true,
            dry_run: false,
            now: now() + chrono::Duration::minutes(6),
        },
    )
    .unwrap();
    let settled = evaluate(store.as_ref(), &host, &trigger, true)
        .state
        .unwrap()
        .active
        .unwrap();
    assert_eq!(settled.state, BatchState::Exhausted);
    assert_eq!(settled.attempt, 2);
    host.failed.store(false, Ordering::SeqCst);

    (store, host, trigger, batch)
}

/// The settings the operator retuned tonight: a lower threshold over the same
/// branch, repository and examination contract.
fn retuned(trigger: &DeliveryTrigger) -> DeliveryTrigger {
    DeliveryTrigger {
        threshold: 1,
        max_wait_minutes: 30,
        ..trigger.clone()
    }
}

#[test]
fn preview_reports_the_stall_and_its_debt_without_writing() {
    let (store, _host, trigger, batch) = stalled();
    let before = store.automation_state(CONSUMER).unwrap();
    let retuned = retuned(&trigger);
    let request = RecoveryRequest::default();

    let preview = recovery::preview(store.as_ref(), &recovery(&retuned, "v2", &request)).unwrap();

    assert_eq!(preview.reason, delivery::DEFINITION_CHANGED);
    assert_eq!(preview.identity.recorded_epoch, "v1");
    assert_eq!(preview.identity.configured_epoch, "v2");
    assert_eq!(
        preview.identity.changes,
        vec!["threshold".to_string(), "max_wait_minutes".to_string()]
    );
    assert_eq!(preview.debt.pending_deliveries, 2);
    assert_eq!(preview.debt.pending_commits, 2);
    assert_eq!(preview.debt.covered, revision(0));
    assert_eq!(preview.debt.receipts, 0);

    let action = preview.action.expect("a settled action is retained");
    assert_eq!(action.batch_id, batch.id);
    assert_eq!(action.obligations, vec![landing(1).key, landing(2).key]);
    assert!(action.reissuable);
    assert!(preview.refusals.is_empty());
    assert!(preview.applied.is_empty());

    assert_eq!(store.automation_state(CONSUMER).unwrap(), before);
    assert!(
        store
            .automation_recoveries(CONSUMER, 10)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn adopting_settings_retains_every_obligation_and_records_the_old_identity() {
    let (store, host, trigger, batch) = stalled();
    let retuned = retuned(&trigger);
    let request = request(true, false);

    let applied = recovery::apply(store.as_ref(), &recovery(&retuned, "v2", &request)).unwrap();
    assert_eq!(applied.applied, vec![RecoveryPreview::ADOPTED_SETTINGS]);
    assert!(
        applied.refusals.is_empty(),
        "a completed recovery reports the position it left, not the request it settled"
    );

    let state = store.automation_state(CONSUMER).unwrap().unwrap();
    assert_eq!(state.epoch, "v2");
    assert_eq!(state.trigger.as_ref(), Some(&retuned));
    assert_eq!(state.covered, revision(0), "adoption covers nothing");
    assert_eq!(state.pending.len(), 2, "retained debt survives adoption");
    assert_eq!(state.pending_commits.len(), 2);
    assert_eq!(
        state.active.as_ref().unwrap().batch,
        batch,
        "the frozen obligations are untouched"
    );

    let record = store.automation_recoveries(CONSUMER, 10).unwrap();
    assert_eq!(record.len(), 1);
    assert_eq!(record[0].previous_epoch, "v1");
    assert_eq!(record[0].epoch, "v2");
    assert_eq!(record[0].previous_trigger.as_ref(), Some(&trigger));
    assert!(record[0].adopted_settings);
    assert!(record[0].reissued.is_none());

    // The adopted consumer evaluates again under its new settings instead of
    // stalling, and still reports the settled action as needing attention.
    let resumed = delivery::evaluate(
        store.as_ref(),
        &host,
        Evaluation {
            consumer: CONSUMER,
            epoch: "v2",
            trigger: &retuned,
            enabled: true,
            dry_run: false,
            now: now(),
        },
    )
    .unwrap();
    assert_eq!(resumed.reason, "needs_attention");
}

#[test]
fn a_reissued_action_admits_a_new_task_and_covers_only_with_fresh_evidence() {
    let (store, host, trigger, batch) = stalled();
    let retuned = retuned(&trigger);
    let settled_action = store
        .automation_state(CONSUMER)
        .unwrap()
        .unwrap()
        .active
        .unwrap()
        .action_id
        .unwrap();
    let request = request(true, true);

    let applied = recovery::apply(store.as_ref(), &recovery(&retuned, "v2", &request)).unwrap();
    assert_eq!(
        applied.applied,
        vec![
            RecoveryPreview::ADOPTED_SETTINGS,
            RecoveryPreview::REISSUED_ACTION
        ]
    );

    let claim = store
        .automation_state(CONSUMER)
        .unwrap()
        .unwrap()
        .active
        .unwrap();
    assert_eq!(
        claim.batch, batch,
        "the same frozen obligations are reissued"
    );
    assert_eq!(claim.attempt, 3);
    assert_eq!(claim.state, BatchState::Claimed);
    assert!(claim.action_id.is_none());
    assert_eq!(
        claim.reissue.as_ref().unwrap().from_action_id.as_ref(),
        Some(&settled_action)
    );

    // The reissued claim admits a new action past the frozen retry deadline,
    // which the operator authorization explicitly extended.
    host.page(2, 2);
    let admitted = delivery::evaluate(
        store.as_ref(),
        &host,
        Evaluation {
            consumer: CONSUMER,
            epoch: "v2",
            trigger: &retuned,
            enabled: true,
            dry_run: false,
            now: now() + chrono::Duration::hours(2),
        },
    )
    .unwrap()
    .state
    .unwrap()
    .active
    .unwrap();
    assert_eq!(admitted.state, BatchState::Admitted);
    assert_ne!(admitted.action_id, Some(settled_action));
    assert_eq!(host.actions.lock().unwrap().len(), 3);
    assert_eq!(
        store.automation_state(CONSUMER).unwrap().unwrap().covered,
        revision(0),
        "authorizing a retry never advances coverage"
    );

    host.evidence(&admitted);
    let covered = delivery::evaluate(
        store.as_ref(),
        &host,
        Evaluation {
            consumer: CONSUMER,
            epoch: "v2",
            trigger: &retuned,
            enabled: true,
            dry_run: false,
            now: now() + chrono::Duration::hours(3),
        },
    )
    .unwrap();
    assert_eq!(covered.state.unwrap().covered, revision(2));
    assert_eq!(covered.receipts.len(), 1);
}

#[test]
fn evidence_frozen_against_the_replaced_attempt_cannot_cover_the_reissue() {
    let (store, host, trigger, _batch) = stalled();
    let stale = store
        .automation_state(CONSUMER)
        .unwrap()
        .unwrap()
        .active
        .unwrap();
    host.evidence(&stale);

    let retuned = retuned(&trigger);
    let request = request(true, true);
    recovery::apply(store.as_ref(), &recovery(&retuned, "v2", &request)).unwrap();

    host.page(2, 2);
    let state = delivery::evaluate(
        store.as_ref(),
        &host,
        Evaluation {
            consumer: CONSUMER,
            epoch: "v2",
            trigger: &retuned,
            enabled: true,
            dry_run: false,
            now: now() + chrono::Duration::hours(2),
        },
    )
    .unwrap();

    assert_eq!(
        state.state.unwrap().covered,
        revision(0),
        "the archived action's evidence names an attempt that no longer exists"
    );
    assert!(state.receipts.is_empty());
}

#[test]
fn incompatible_and_unauthorized_changes_are_refused_without_touching_state() {
    let (store, _host, trigger, _batch) = stalled();
    let before = store.automation_state(CONSUMER).unwrap();

    let mut branch = retuned(&trigger);
    branch.branch = "main".into();
    let mut coverage = retuned(&trigger);
    coverage.coverage = CoverageClass::LandedCodeReviewV1;
    let mut owner = retuned(&trigger);
    owner.owner_machine = Some("another-machine".into());

    let adopt = request(true, false);
    let unexplained = RecoveryRequest {
        reason: "   ".into(),
        ..request(true, false)
    };

    for (expected, trigger, epoch, request, repository) in [
        (refusal::BRANCH_CHANGED, &branch, "v2", &adopt, "owner/repo"),
        (
            refusal::COVERAGE_CHANGED,
            &coverage,
            "v2",
            &adopt,
            "owner/repo",
        ),
        (refusal::OWNER_CHANGED, &owner, "v2", &adopt, "owner/repo"),
        (
            refusal::REPOSITORY_CHANGED,
            &branch,
            "v2",
            &adopt,
            "owner/moved",
        ),
        (
            refusal::MISSING_AUTHORIZATION,
            &retuned(&trigger),
            "v2",
            &unexplained,
            "owner/repo",
        ),
        (
            refusal::SETTINGS_UNCHANGED,
            &trigger,
            "v1",
            &adopt,
            "owner/repo",
        ),
    ] {
        let mut recovery = recovery(trigger, epoch, request);
        recovery.repository = repository;

        let error = recovery::apply(store.as_ref(), &recovery).expect_err("refused");
        let AutomationError::Refused(reasons) = error else {
            panic!("expected a typed refusal, got {error}");
        };
        assert!(reasons.contains(expected), "{reasons} lacks {expected}");
        assert_eq!(store.automation_state(CONSUMER).unwrap(), before);
        assert!(
            store
                .automation_recoveries(CONSUMER, 10)
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn a_live_action_and_an_evidenced_batch_are_never_recovered() {
    let (store, host, trigger) = setup();
    host.page(0, 2);
    let admitted = evaluate(store.as_ref(), &host, &trigger, true)
        .state
        .unwrap()
        .active
        .unwrap();
    assert_eq!(admitted.state, BatchState::Admitted);

    let retuned = retuned(&trigger);
    let request = request(true, true);
    let error = recovery::apply(store.as_ref(), &recovery(&retuned, "v2", &request))
        .expect_err("a running action is never interrupted");
    let AutomationError::Refused(reasons) = error else {
        panic!("expected a typed refusal, got {error}");
    };
    assert!(reasons.contains(refusal::ACTIVE_EXECUTION), "{reasons}");
    assert!(reasons.contains(refusal::NO_SETTLED_ACTION), "{reasons}");

    // Once the batch is covered there is nothing left to reissue either.
    host.evidence(&admitted);
    host.page(2, 2);
    assert_eq!(
        evaluate(store.as_ref(), &host, &trigger, true)
            .state
            .unwrap()
            .covered,
        revision(2)
    );
    let error = recovery::apply(store.as_ref(), &recovery(&retuned, "v2", &request))
        .expect_err("an evidenced batch is not reissuable");
    let AutomationError::Refused(reasons) = error else {
        panic!("expected a typed refusal, got {error}");
    };
    assert!(reasons.contains(refusal::NO_SETTLED_ACTION), "{reasons}");
    assert_eq!(
        store.automation_state(CONSUMER).unwrap().unwrap().epoch,
        "v1",
        "one refused operation refuses the whole request"
    );
}

#[test]
fn a_reissue_alone_cannot_strand_a_claim_under_a_stale_identity() {
    let (store, _host, trigger, _batch) = stalled();
    let retuned = retuned(&trigger);
    let request = request(false, true);

    let error = recovery::apply(store.as_ref(), &recovery(&retuned, "v2", &request))
        .expect_err("a claim under a stale identity would never admit");
    let AutomationError::Refused(reasons) = error else {
        panic!("expected a typed refusal, got {error}");
    };
    assert!(reasons.contains(refusal::DEFINITION_CHANGED), "{reasons}");
}

#[test]
fn an_unknown_consumer_and_a_host_refusal_stop_before_any_write() {
    let (store, _host, trigger, _batch) = stalled();
    let retuned = retuned(&trigger);
    let request = request(true, false);

    let mut unknown = recovery(&retuned, "v2", &request);
    unknown.consumer = "ws/absent";
    let error = recovery::apply(store.as_ref(), &unknown).expect_err("nothing to recover");
    assert!(matches!(
        error,
        AutomationError::Refused(reasons) if reasons == refusal::UNKNOWN_CONSUMER
    ));

    let mut elsewhere = recovery(&retuned, "v2", &request);
    elsewhere.host_refusal = Some(refusal::OWNED_ELSEWHERE);
    let error = recovery::apply(store.as_ref(), &elsewhere).expect_err("not this host's consumer");
    let AutomationError::Refused(reasons) = error else {
        panic!("expected a typed refusal, got {error}");
    };
    assert!(reasons.contains(refusal::OWNED_ELSEWHERE), "{reasons}");
    assert!(
        store
            .automation_recoveries(CONSUMER, 10)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn history_replay_preserves_frozen_coverage_and_adds_canonical_debt() {
    let (store, _host, trigger, frozen_batch) = stalled();
    let before = store.automation_state(CONSUMER).unwrap().unwrap();
    let orphan = before.observed.clone();
    let inserted = SourceRevision {
        commit: "69555b04".into(),
        tree: "inserted-tree".into(),
    };
    let canonical = SourceRevision {
        commit: "50798789".into(),
        tree: orphan.tree.clone(),
    };
    let mut replacement = landing(2);
    replacement.before = inserted.clone();
    replacement.after = canonical.clone();
    replacement.commits = vec![canonical.commit.clone()];
    replacement.evidence_digest = "canonical-proof".into();
    let added = Delivery {
        key: "pr:owner/repo:agent-main:1586".into(),
        repository: before.repository.clone(),
        branch: before.branch.clone(),
        before: before.covered.clone(),
        after: inserted.clone(),
        commits: vec![inserted.commit.clone()],
        task_ids: vec!["ORB-11751".into()],
        evidence_reference: "https://example.test/1586".into(),
        evidence_digest: "inserted-proof".into(),
        landed_at: now(),
    };
    let page = SourcePage {
        from: before.covered.clone(),
        through: canonical.clone(),
        commits: vec![inserted.commit.clone(), canonical.commit.clone()],
        deliveries: vec![added.clone(), replacement],
        unresolved: Default::default(),
        associations: Default::default(),
        exclusions: Default::default(),
        complete: true,
    };
    let record = HistoryReplayRecord {
        captured_generation: before.generation,
        captured_head: SourceRevision {
            commit: "ac0429ba".into(),
            tree: "current-tree".into(),
        },
        common_base: before.covered.clone(),
        old_observed: orphan.clone(),
        new_observed: canonical.clone(),
        mappings: vec![HistoryMapping {
            orphan,
            canonical: canonical.clone(),
            proof_digest: "orbit-tree-and-binary-patch".into(),
        }],
        added_obligations: vec![],
        unchanged_baseline: before.baseline.clone(),
        unchanged_covered: before.covered.clone(),
        accepted_receipts: 0,
    };
    let request = RecoveryRequest {
        replay_history: true,
        reason: "reconcile the verified Sep 8 rebase".into(),
        ..Default::default()
    };
    let operation = recovery::Recovery {
        consumer: CONSUMER,
        epoch: "v1",
        trigger: &trigger,
        repository: "owner/repo",
        host_refusal: None,
        request: &request,
        by: "operator",
        now: now(),
        replay: Some(recovery::HistoryReplayInput { page, record }),
    };

    let preview = recovery::preview(store.as_ref(), &operation).unwrap();
    assert_eq!(
        store.automation_state(CONSUMER).unwrap(),
        Some(before.clone())
    );
    assert_eq!(
        preview.history_replay.unwrap().added_obligations,
        vec![added.key.clone()]
    );

    let applied = recovery::apply(store.as_ref(), &operation).unwrap();
    assert_eq!(applied.applied, vec![RecoveryPreview::REPLAYED_HISTORY]);
    let after = store.automation_state(CONSUMER).unwrap().unwrap();
    assert_eq!(after.baseline, before.baseline);
    assert_eq!(after.covered, before.covered);
    assert_eq!(after.waived, before.waived);
    assert_eq!(after.excluded, before.excluded);
    assert_eq!(after.active, before.active);
    assert_eq!(after.active.unwrap().batch, frozen_batch);
    assert_eq!(after.observed, canonical);
    assert!(
        after
            .pending
            .iter()
            .any(|delivery| delivery.key == added.key)
    );
    assert_eq!(
        store.automation_recoveries(CONSUMER, 10).unwrap()[0]
            .replayed_history
            .as_ref()
            .unwrap()
            .added_obligations,
        vec![added.key]
    );

    let settled = store.automation_state(CONSUMER).unwrap();
    assert!(matches!(
        recovery::apply(store.as_ref(), &operation),
        Err(AutomationError::Refused(reason)) if reason == refusal::HISTORY_CONTRACT_DRIFT
    ));
    assert_eq!(store.automation_state(CONSUMER).unwrap(), settled);
    assert_eq!(store.automation_recoveries(CONSUMER, 10).unwrap().len(), 1);
}
