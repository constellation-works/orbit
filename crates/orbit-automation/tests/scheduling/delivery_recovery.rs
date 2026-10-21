//! Recovery keeps every obligation and never substitutes authorization for
//! evidence [ORB-12295].

use super::delivery::{
    CONSUMER, Host, evaluate, evaluate_at, landing, now, revision, setup, trigger,
};
use orbit_automation::{
    AutomationError,
    delivery::{self, recovery, reset},
};
use orbit_store::contracts::AutomationStoreBackend;
use orbit_types::workflow::automation::recovery::{
    HistoryMapping, HistoryReplayRecord, RecoveryPreview, RecoveryRecord, RecoveryRequest,
    ResetRequest, refusal,
};
use orbit_types::workflow::automation::*;
use std::sync::atomic::Ordering;

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
        resolved_action_id: None,
        expected_generation: None,
        action_terminal: false,
        action_failed_without_evidence: false,
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
    evaluate_at(store.as_ref(), &host, &trigger, true, 6).unwrap();
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

/// A consumer still holding an admitted action whose task closed with
/// malformed coverage — the state an evaluation never reached to settle.
fn wedged() -> (
    std::sync::Arc<dyn AutomationStoreBackend>,
    Host,
    DeliveryTrigger,
    AutomationState,
) {
    let (store, host, trigger) = setup();
    host.page(0, 2);
    evaluate(store.as_ref(), &host, &trigger, true);
    *host.raw_evidence.lock().unwrap() = Some(br#"{"schema_version":1,"batch_id":{}}"#.to_vec());
    host.stopped.store(true, Ordering::SeqCst);
    let state = store.automation_state(CONSUMER).unwrap().unwrap();
    assert_eq!(state.active.as_ref().unwrap().state, BatchState::Admitted);

    (store, host, trigger, state)
}

#[test]
fn a_minted_action_is_stopped_only_once_its_task_closed_without_acceptable_evidence() {
    for batch_state in [BatchState::Claimed, BatchState::Admitted] {
        let (_store, host, _trigger, mut state) = wedged();
        state.active.as_mut().unwrap().state = batch_state;
        let liveness = delivery::action_liveness(&host, &state, now()).unwrap();
        assert!(liveness.terminal);
        assert!(liveness.failed_without_evidence);

        // The executor can still replace malformed bytes while its task is open.
        host.stopped.store(false, Ordering::SeqCst);
        let open = delivery::action_liveness(&host, &state, now()).unwrap();
        assert!(!open.terminal);
        assert!(!open.failed_without_evidence);

        // Valid evidence on a closed task is settled by acceptance, not failure.
        host.stopped.store(true, Ordering::SeqCst);
        *host.raw_evidence.lock().unwrap() = None;
        host.evidence(state.active.as_ref().unwrap());
        let accepted = delivery::action_liveness(&host, &state, now()).unwrap();
        assert!(accepted.terminal);
        assert!(!accepted.failed_without_evidence);
    }
}

#[test]
fn recovery_reissues_an_admitted_action_whose_task_closed_without_evidence() {
    let (store, _host, trigger, state) = wedged();
    let reissue = request(false, true);

    let mut executing = recovery(&trigger, "v1", &reissue);
    let error = recovery::apply(store.as_ref(), &executing).expect_err("live action refused");
    let AutomationError::Refused(reasons) = error else {
        panic!("expected a typed refusal, got {error}");
    };
    assert!(reasons.contains(refusal::ACTIVE_EXECUTION), "{reasons}");

    executing.action_terminal = true;
    executing.action_failed_without_evidence = true;
    let preview = recovery::preview(store.as_ref(), &executing).unwrap();
    assert_eq!(preview.reason, "needs_attention");
    assert!(preview.action.as_ref().unwrap().reissuable);

    let applied = recovery::apply(store.as_ref(), &executing).unwrap();
    assert_eq!(
        applied.applied,
        vec![RecoveryPreview::REISSUED_ACTION.to_string()]
    );
    let after = store.automation_state(CONSUMER).unwrap().unwrap();
    let attempt = after.active.unwrap();
    assert_eq!(attempt.state, BatchState::Claimed);
    assert_eq!(attempt.attempt, 2);
    assert_eq!(attempt.batch, state.active.unwrap().batch);
    assert_eq!(after.covered, revision(0));
}

/// Deterministic interleaving: admission changes the action after the host's
/// terminal probe and before recovery loads its checkpoint.
#[test]
fn a_terminal_proof_cannot_reissue_a_concurrently_admitted_replacement() {
    let (store, _host, trigger, state) = wedged();
    let reissue = request(false, true);
    let mut operation = recovery(&trigger, "v1", &reissue);
    operation.expected_generation = Some(state.generation);
    operation.action_terminal = true;
    operation.action_failed_without_evidence = true;

    let mut admitted = state.clone();
    admitted.generation += 1;
    let active = admitted.active.as_mut().unwrap();
    active.attempt += 1;
    active.action_key = format!("automation:{}:{}", active.batch.id, active.attempt);
    active.action_id = Some("replacement-action".into());
    assert!(store.automation_commit(&state, &admitted, None).unwrap());

    let error = recovery::apply(store.as_ref(), &operation).unwrap_err();
    assert!(
        matches!(error, AutomationError::Deferred(reason) if reason == "concurrent_evaluation")
    );
    assert_eq!(store.automation_state(CONSUMER).unwrap(), Some(admitted));
    assert!(
        store
            .automation_recoveries(CONSUMER, 10)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn recovery_does_not_reissue_a_terminal_action_with_acceptable_evidence() {
    let (store, host, trigger, state) = wedged();
    *host.raw_evidence.lock().unwrap() = None;
    host.evidence(state.active.as_ref().unwrap());
    let liveness = delivery::action_liveness(&host, &state, now()).unwrap();
    assert!(liveness.terminal);
    assert!(!liveness.failed_without_evidence);

    let reissue = request(false, true);
    let mut operation = recovery(&trigger, "v1", &reissue);
    operation.action_terminal = liveness.terminal;
    operation.action_failed_without_evidence = liveness.failed_without_evidence;
    let error =
        recovery::apply(store.as_ref(), &operation).expect_err("valid evidence is not reissued");
    let AutomationError::Refused(reasons) = error else {
        panic!("expected a typed refusal, got {error}");
    };
    assert!(!reasons.contains(refusal::ACTIVE_EXECUTION), "{reasons}");
    assert!(reasons.contains(refusal::NO_SETTLED_ACTION), "{reasons}");
}

#[test]
fn reset_forgets_a_consumer_whose_admitted_action_already_stopped() {
    let (store, _host, trigger, state) = wedged();
    let request = ResetRequest {
        reason: "the review task closed with malformed coverage".into(),
        force: false,
    };
    let mut operation = reset::Reset {
        consumer: CONSUMER,
        epoch: "v1",
        trigger: &trigger,
        host_refusal: None,
        request: &request,
        by: "operator",
        now: now(),
        baseline: revision(2),
        released_refs: vec![],
        inspected_state: Some(&state),
        action_terminal: false,
        action_failed_without_evidence: false,
    };

    let error = reset::apply(store.as_ref(), &operation).expect_err("live action refused");
    let AutomationError::Refused(reasons) = error else {
        panic!("expected a typed refusal, got {error}");
    };
    assert!(reasons.contains(refusal::ACTION_EXECUTING), "{reasons}");
    assert_eq!(
        store.automation_state(CONSUMER).unwrap(),
        Some(state.clone())
    );

    operation.action_terminal = true;
    operation.action_failed_without_evidence = true;
    let applied = reset::apply(store.as_ref(), &operation).unwrap();
    assert!(applied.applied);
    assert!(store.automation_state(CONSUMER).unwrap().is_none());
}

#[test]
fn reset_can_forget_a_terminal_action_with_acceptable_evidence() {
    let (store, host, trigger, state) = wedged();
    *host.raw_evidence.lock().unwrap() = None;
    host.evidence(state.active.as_ref().unwrap());
    let liveness = delivery::action_liveness(&host, &state, now()).unwrap();
    assert!(liveness.terminal);
    assert!(!liveness.failed_without_evidence);

    let request = ResetRequest {
        reason: "the review task has already closed".into(),
        force: false,
    };
    let operation = reset::Reset {
        consumer: CONSUMER,
        epoch: "v1",
        trigger: &trigger,
        host_refusal: None,
        request: &request,
        by: "operator",
        now: now(),
        baseline: revision(2),
        released_refs: vec![],
        inspected_state: Some(&state),
        action_terminal: liveness.terminal,
        action_failed_without_evidence: liveness.failed_without_evidence,
    };
    let applied = reset::apply(store.as_ref(), &operation).unwrap();
    assert!(applied.applied);
    assert!(!applied.action.unwrap().reissuable);
    assert!(store.automation_state(CONSUMER).unwrap().is_none());
}

/// The retry is admitted through the public evaluator, with virtual backoff.
/// No timing race or synthetic load is needed to move beyond the probed action.
#[test]
fn reset_cannot_inherit_terminal_proof_across_retry_admission() {
    let (store, host, trigger, inspected) = wedged();
    let liveness = delivery::action_liveness(&host, &inspected, now()).unwrap();
    assert!(liveness.terminal);
    let request = ResetRequest {
        reason: "discard the closed action's debt".into(),
        force: false,
    };
    let operation = reset::Reset {
        consumer: CONSUMER,
        epoch: "v1",
        trigger: &trigger,
        host_refusal: None,
        request: &request,
        by: "operator",
        now: now(),
        baseline: revision(2),
        released_refs: vec!["refs/orbit/automation/fixture/batch/through".into()],
        inspected_state: Some(&inspected),
        action_terminal: liveness.terminal,
        action_failed_without_evidence: liveness.failed_without_evidence,
    };

    host.page(2, 2);
    evaluate(store.as_ref(), &host, &trigger, true);
    host.stopped.store(false, Ordering::SeqCst);
    *host.raw_evidence.lock().unwrap() = None;
    let replacement = evaluate_at(store.as_ref(), &host, &trigger, true, 6)
        .unwrap()
        .state
        .unwrap();
    assert!(replacement.generation > inspected.generation);
    let active = replacement.active.as_ref().unwrap();
    assert_eq!(active.state, BatchState::Admitted);
    assert_ne!(
        active.action_id,
        inspected.active.as_ref().unwrap().action_id
    );
    assert!(
        !delivery::action_liveness(&host, &replacement, now())
            .unwrap()
            .terminal
    );

    for result in [
        reset::preview(store.as_ref(), &operation),
        reset::apply(store.as_ref(), &operation),
    ] {
        assert!(matches!(result,
            Err(AutomationError::Deferred(reason)) if reason == "concurrent_evaluation"));
        assert_eq!(
            store.automation_state(CONSUMER).unwrap(),
            Some(replacement.clone())
        );
        assert!(
            store
                .automation_recoveries(CONSUMER, 10)
                .unwrap()
                .is_empty()
        );
    }
}

/// The canonical counterpart of fixture commit `n` after a content-preserving
/// rebase onto the same base: a new commit carrying the same tree.
fn rebased(n: usize) -> SourceRevision {
    SourceRevision {
        commit: format!("r{n}"),
        tree: revision(n).tree,
    }
}

/// Landing `n` as the provider restates it on the rebased history.
fn rebased_landing(n: usize) -> Delivery {
    Delivery {
        before: if n == 1 { revision(0) } else { rebased(n - 1) },
        after: rebased(n),
        commits: vec![rebased(n).commit],
        evidence_digest: format!("rebased-evidence-{n}"),
        ..landing(n)
    }
}

fn certificate(n: usize) -> DeliveryExclusion {
    DeliveryExclusion {
        attempt_id: format!("certificate-attempt-{n}"),
        assurance: "reviewed".into(),
        task_meaning_digest: format!("meaning-{n}"),
        final_candidate_tree: revision(n).tree,
    }
}

fn review_setup() -> (
    std::sync::Arc<dyn AutomationStoreBackend>,
    Host,
    DeliveryTrigger,
) {
    let store =
        orbit_store::compose::automation_store(orbit_store::Store::open_in_memory().unwrap())
            .unwrap();
    let host = Host::new();
    let trigger = DeliveryTrigger {
        coverage: CoverageClass::LandedCodeReviewV1,
        retries: 0,
        ..trigger()
    };
    assert_eq!(
        evaluate(store.as_ref(), &host, &trigger, true).reason,
        "baselined"
    );
    (store, host, trigger)
}

/// A proven replay of every retained commit `cN` onto `rN`, restating
/// `deliveries` and keeping each listed commit's unresolved reason.
fn replay_input(
    state: &AutomationState,
    deliveries: Vec<Delivery>,
    unresolved: &[usize],
) -> recovery::HistoryReplayInput {
    let count = state.pending_commits.len();
    assert_eq!(
        state.pending_commits,
        (1..=count).map(|n| revision(n).commit).collect::<Vec<_>>()
    );
    recovery::HistoryReplayInput {
        page: SourcePage {
            from: revision(0),
            through: rebased(count),
            commits: (1..=count).map(|n| rebased(n).commit).collect(),
            deliveries,
            unresolved: unresolved
                .iter()
                .map(|&n| {
                    (
                        rebased(n).commit,
                        state.unresolved[&revision(n).commit].clone(),
                    )
                })
                .collect(),
            associations: Default::default(),
            lookup_retries: Default::default(),
            exclusions: Default::default(),
            complete: true,
        },
        record: HistoryReplayRecord {
            captured_generation: state.generation,
            captured_head: rebased(count),
            common_base: revision(0),
            old_observed: state.observed.clone(),
            new_observed: rebased(count),
            mappings: (1..=count)
                .map(|n| HistoryMapping {
                    orphan: revision(n),
                    canonical: rebased(n),
                    proof_digest: format!("proof-{n}"),
                })
                .collect(),
            added_obligations: vec![],
            unchanged_baseline: state.baseline.clone(),
            unchanged_covered: state.covered.clone(),
            accepted_receipts: 0,
        },
    }
}

fn replay(
    store: &dyn AutomationStoreBackend,
    trigger: &DeliveryTrigger,
    input: recovery::HistoryReplayInput,
) -> Result<RecoveryPreview, AutomationError> {
    let request = RecoveryRequest {
        replay_history: true,
        reason: "reconcile the verified content-preserving rebase".into(),
        ..RecoveryRequest::default()
    };
    recovery::apply(
        store,
        &recovery::Recovery {
            replay: Some(input),
            ..recovery(trigger, "v1", &request)
        },
    )
}

/// Unresolved commit 1 ahead of certificate-excluded landing 2: the exclusion
/// is retained because the unresolved commit blocks its prefix.
fn unresolved_then_excluded() -> (
    std::sync::Arc<dyn AutomationStoreBackend>,
    Host,
    DeliveryTrigger,
    AutomationState,
) {
    let (store, host, trigger) = review_setup();
    host.page(0, 2);
    {
        let mut page = host.page.lock().unwrap();
        page.deliveries.remove(0);
        page.unresolved
            .insert(revision(1).commit, "provider_identity_missing".into());
        page.exclusions.insert(landing(2).key, certificate(2));
    }
    let state = evaluate(store.as_ref(), &host, &trigger, true)
        .state
        .unwrap();
    assert_eq!(state.covered, revision(0));
    assert_eq!(state.excluded.len(), 1);
    assert_eq!(state.unresolved.len(), 1);
    (store, host, trigger, state)
}

#[test]
fn replayed_exclusions_still_retire_their_canonical_covered_prefix() {
    let (store, host, trigger, before) = unresolved_then_excluded();

    replay(
        store.as_ref(),
        &trigger,
        replay_input(&before, vec![rebased_landing(2)], &[1]),
    )
    .expect("a proven replay applies");
    let replayed = store.automation_state(CONSUMER).unwrap().unwrap();
    assert_eq!(replayed.pending_commits, ["r1", "r2"]);
    let excluded = &replayed.excluded[0];
    assert_eq!(excluded.delivery.commits, ["r2"]);
    assert_eq!(excluded.delivery.after, rebased(2));
    assert_eq!(
        (&excluded.exclusion, excluded.decided_at),
        (&before.excluded[0].exclusion, before.excluded[0].decided_at),
        "the certificate provenance stays auditable"
    );

    // The provider now resolves r1 as another certificate-covered landing.
    *host.page.lock().unwrap() = SourcePage {
        from: rebased(2),
        through: rebased(2),
        commits: vec![],
        deliveries: vec![rebased_landing(1)],
        unresolved: Default::default(),
        associations: Default::default(),
        lookup_retries: Default::default(),
        exclusions: [(landing(1).key, certificate(1))].into(),
        complete: true,
    };
    let state = evaluate(store.as_ref(), &host, &trigger, true)
        .state
        .unwrap();
    assert_eq!(
        state.covered,
        rebased(2),
        "the whole canonical excluded prefix retires, not just r1"
    );
    assert!(state.pending_commits.is_empty());
    assert!(state.excluded.is_empty());
    assert!(store.automation_receipts(CONSUMER, 10).unwrap().is_empty());
    assert!(host.actions.lock().unwrap().is_empty());
}

#[test]
fn replayed_waivers_and_exclusions_retire_with_a_later_accepted_batch() {
    let (store, host, trigger) = review_setup();

    // Landings 1 and 2 are waived after their only review attempt failed.
    host.page(0, 2);
    evaluate(store.as_ref(), &host, &trigger, true);
    host.failed.store(true, Ordering::SeqCst);
    host.page(2, 2);
    let exhausted = evaluate(store.as_ref(), &host, &trigger, true)
        .state
        .unwrap()
        .active
        .unwrap();
    assert_eq!(exhausted.state, BatchState::Exhausted);
    host.failed.store(false, Ordering::SeqCst);
    delivery::waive(
        store.as_ref(),
        CONSUMER,
        &WaiveBatchRequest {
            batch_id: exhausted.batch.id.clone(),
            reason: "the failed review is examined by hand".into(),
        },
        "operator",
        now(),
    )
    .unwrap();

    // Excluded landing 3 sits between the waived gap and pending landing 4.
    host.page(2, 4);
    host.page
        .lock()
        .unwrap()
        .exclusions
        .insert(landing(3).key, certificate(3));
    let before = evaluate(store.as_ref(), &host, &trigger, true)
        .state
        .unwrap();
    assert_eq!(
        (
            before.waived.len(),
            before.excluded.len(),
            before.pending.len()
        ),
        (2, 1, 1)
    );

    replay(
        store.as_ref(),
        &trigger,
        replay_input(&before, (1..=4).map(rebased_landing).collect(), &[]),
    )
    .expect("a proven replay applies");
    let replayed = store.automation_state(CONSUMER).unwrap().unwrap();
    assert_eq!(
        replayed
            .waived
            .iter()
            .map(|delivery| delivery.commits.clone())
            .collect::<Vec<_>>(),
        [["r1"], ["r2"]],
        "the waived gap stays a waiver, on canonical commits"
    );
    assert_eq!(replayed.excluded[0].delivery.commits, ["r3"]);
    assert_eq!(replayed.pending[0].commits, ["r4"]);
    assert_eq!(
        store.automation_waivers(CONSUMER, 10).unwrap()[0].batch_id,
        exhausted.batch.id,
        "the waiver record is untouched"
    );

    // Landing 5 makes a batch due; the exclusion travels with it as context.
    *host.page.lock().unwrap() = SourcePage {
        from: rebased(4),
        through: rebased(5),
        commits: vec![rebased(5).commit],
        deliveries: vec![rebased_landing(5)],
        unresolved: Default::default(),
        associations: Default::default(),
        lookup_retries: Default::default(),
        exclusions: Default::default(),
        complete: true,
    };
    let attempt = evaluate(store.as_ref(), &host, &trigger, true)
        .state
        .unwrap()
        .active
        .unwrap();
    assert_eq!(attempt.batch.commits, ["r1", "r2", "r3", "r4", "r5"]);
    assert_eq!(attempt.batch.exclusions, replayed.excluded);

    host.evidence(&attempt);
    *host.page.lock().unwrap() = SourcePage {
        from: rebased(5),
        through: rebased(5),
        commits: vec![],
        deliveries: vec![],
        unresolved: Default::default(),
        associations: Default::default(),
        lookup_retries: Default::default(),
        exclusions: Default::default(),
        complete: true,
    };
    let state = evaluate(store.as_ref(), &host, &trigger, true)
        .state
        .unwrap();
    assert_eq!(state.covered, rebased(5));
    assert!(state.pending_commits.is_empty());
    assert!(
        state.waived.is_empty(),
        "no stale waiver survives the batch"
    );
    assert!(
        state.excluded.is_empty(),
        "no stale exclusion survives the batch"
    );
    assert_eq!(store.automation_receipts(CONSUMER, 10).unwrap().len(), 1);
}

#[test]
fn a_retained_exclusion_the_proof_cannot_restate_refuses_the_replay() {
    let (store, _host, trigger, before) = unresolved_then_excluded();

    let moved = Delivery {
        evidence_reference: "https://github.com/owner/repo/pull/99".into(),
        ..rebased_landing(2)
    };
    let regrouped = Delivery {
        commits: vec!["r1".into(), "r2".into()],
        ..rebased_landing(2)
    };
    // Omitted, re-identified, or regrouped over the unresolved commit.
    for (deliveries, expected) in [
        (vec![], refusal::HISTORY_CONTRACT_DRIFT),
        (vec![moved], refusal::HISTORY_CONTRACT_DRIFT),
        (vec![regrouped], refusal::PROVIDER_PROOF_UNAVAILABLE),
    ] {
        let error = replay(
            store.as_ref(),
            &trigger,
            replay_input(&before, deliveries, &[1]),
        )
        .expect_err("unproven membership is refused");
        assert!(
            matches!(&error, AutomationError::Refused(reason) if reason == expected),
            "{error}"
        );
        assert_eq!(
            store.automation_state(CONSUMER).unwrap(),
            Some(before.clone())
        );
        assert!(
            store
                .automation_recoveries(CONSUMER, 10)
                .unwrap()
                .is_empty()
        );
    }

    // The store refuses a replay checkpoint that moves an exclusion anywhere
    // but along the mapping, or rewrites its certificate, and accepts the
    // canonical one.
    let input = replay_input(&before, vec![rebased_landing(2)], &[1]);
    let mut canonical = before.clone();
    canonical.generation += 1;
    canonical.observed = rebased(2);
    canonical.pending_commits = vec!["r1".into(), "r2".into()];
    canonical.unresolved = [("r1".into(), "provider_identity_missing".into())].into();
    canonical.excluded[0].delivery = Delivery {
        task_ids: before.excluded[0].delivery.task_ids.clone(),
        ..rebased_landing(2)
    };
    let record = RecoveryRecord {
        consumer: CONSUMER.into(),
        previous_epoch: before.epoch.clone(),
        epoch: before.epoch.clone(),
        previous_trigger: before.trigger.clone(),
        trigger: before.trigger.clone(),
        adopted_settings: false,
        reissued: None,
        replayed_history: Some(input.record),
        reset: None,
        friction_id: None,
        reason: "reconcile the verified content-preserving rebase".into(),
        by: "operator".into(),
        at: now(),
    };
    let mut misplaced = canonical.clone();
    misplaced.excluded[0].delivery.commits = vec!["r1".into()];
    let mut recertified = canonical.clone();
    recertified.excluded[0].exclusion.attempt_id = "another-certificate".into();
    let mut orphaned = canonical.clone();
    orphaned.excluded = before.excluded.clone();
    let mut dropped = canonical.clone();
    dropped.excluded.clear();
    for forged in [misplaced, recertified, orphaned, dropped] {
        assert!(
            store.automation_recover(&before, &forged, &record).is_err(),
            "{:?}",
            forged.excluded
        );
    }
    assert!(
        store
            .automation_recover(&before, &canonical, &record)
            .unwrap()
    );
}
