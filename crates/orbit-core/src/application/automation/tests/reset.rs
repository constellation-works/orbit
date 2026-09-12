//! Audited reset of a delivery consumer, against a real Git fixture.
//!
//! Reset is the operation that forgets debt, so what it prints before it is
//! authorized matters as much as what it writes afterwards: the preview is the
//! operator's only chance to see the inventory that is about to disappear.

use super::consumer::{commit, definition, git, runtime};
use crate::OrbitRuntime;
use crate::application::automation::{
    consumer_key, evaluate_auto_task, record_direct_landing_intent,
};
use chrono::Utc;
use orbit_types::workflow::AutoTaskDefinition;
use orbit_types::workflow::automation::recovery::{RecoveryRequest, ResetRequest};
use orbit_types::workflow::automation::{
    AutomationState, CoverageClass, DirectLandingRequest, SourceRevision, members::MemberState,
};
use serde_json::json;

fn reason(text: &str) -> ResetRequest {
    ResetRequest {
        reason: text.into(),
        force: false,
    }
}

fn pinned_refs(runtime: &OrbitRuntime) -> Vec<String> {
    git(
        &runtime.paths().repo_root,
        &[
            "for-each-ref",
            "--format=%(refname)",
            "refs/orbit/automation/",
        ],
    )
    .lines()
    .map(str::to_owned)
    .collect()
}

/// Land a commit the consumer has to examine, and let it admit the batch.
fn admitted(runtime: &OrbitRuntime, definition: &AutoTaskDefinition) -> String {
    let baseline = evaluate_auto_task(runtime, definition, false, Utc::now())
        .unwrap()
        .state
        .unwrap()
        .baseline;
    let landed = commit(&runtime.paths().repo_root, "unexamined change");
    let delivery = runtime
        .stores()
        .jobs()
        .insert_job_run("direct_fixture", 1, Utc::now(), Some(json!({})), None)
        .unwrap();
    record_direct_landing_intent(
        runtime,
        &DirectLandingRequest {
            run_id: delivery.run_id,
            branch: "agent-main".into(),
            before_commit: baseline.commit,
            after_commit: landed.clone(),
        },
    )
    .unwrap();
    evaluate_auto_task(runtime, definition, false, Utc::now()).unwrap();

    landed
}

#[test]
fn a_preview_names_the_debt_it_would_forget_and_writes_nothing() {
    let runtime = runtime();
    let qa = definition(&runtime, "delivery-qa", CoverageClass::IntegratedQaV1);
    let landed = admitted(&runtime, &qa);

    let consumer = consumer_key(&runtime, "auto-task", &qa.name).unwrap();
    let store = runtime.automation_store().unwrap();
    let before = store.automation_state(&consumer).unwrap().unwrap();
    let head = git(
        &runtime.paths().repo_root,
        &["rev-parse", "refs/heads/agent-main"],
    );

    let preview =
        super::super::reset_auto_task(&runtime, &qa, &ResetRequest::default(), Utc::now()).unwrap();

    assert_eq!(preview.consumer, consumer);
    assert_eq!(preview.generation, before.generation);
    assert_eq!(preview.epoch, before.epoch);
    assert_eq!(preview.debt.pending_deliveries, 1);
    assert_eq!(preview.debt.pending_commits, 1);
    assert_eq!(preview.debt.receipts, 0);
    assert_eq!(preview.baseline.commit, head);
    assert_eq!(preview.baseline.commit, landed);
    let action = preview.action.as_ref().expect("the batch is executing");
    assert_eq!(action.commits, 1);
    assert_eq!(
        preview.refusals,
        vec!["action_executing".to_string()],
        "the preview already says what an apply would refuse"
    );
    assert!(!preview.applied);

    assert_eq!(
        store.automation_state(&consumer).unwrap(),
        Some(before),
        "a preview is read-only"
    );
    assert!(
        store
            .automation_recoveries(&consumer, 10)
            .unwrap()
            .is_empty(),
        "and writes no audit record"
    );
}

#[test]
fn an_executing_action_refuses_the_reset_until_it_is_forced() {
    let runtime = runtime();
    let qa = definition(&runtime, "delivery-qa", CoverageClass::IntegratedQaV1);
    admitted(&runtime, &qa);

    let consumer = consumer_key(&runtime, "auto-task", &qa.name).unwrap();
    let store = runtime.automation_store().unwrap();
    let before = store.automation_state(&consumer).unwrap().unwrap();
    assert!(!pinned_refs(&runtime).is_empty(), "the batch is pinned");

    let refused = super::super::reset_auto_task(
        &runtime,
        &qa,
        &reason("the branch was rewritten"),
        Utc::now(),
    )
    .expect_err("an executing action is not destroyed silently");
    assert!(
        refused.to_string().contains("action_executing"),
        "{refused}"
    );
    assert_eq!(store.automation_state(&consumer).unwrap(), Some(before));

    let applied = super::super::reset_auto_task(
        &runtime,
        &qa,
        &ResetRequest {
            reason: "the branch was rewritten past the frozen batch".into(),
            force: true,
        },
        Utc::now(),
    )
    .unwrap();
    assert!(applied.applied);
    assert_eq!(applied.debt.pending_deliveries, 1);

    let records = store.automation_recoveries(&consumer, 10).unwrap();
    assert_eq!(records.len(), 1);
    let record = &records[0];
    assert_eq!(record.kind(), "reset");
    assert!(!record.by.is_empty());
    assert_eq!(
        record.reason,
        "the branch was rewritten past the frozen batch"
    );
    let reset = record.reset.as_ref().unwrap();
    assert_eq!(reset.previous_generation, 3);
    assert_eq!(reset.forgotten.pending_deliveries, 1);
    assert_eq!(reset.forgotten.pending_commits, 1);
    assert_eq!(reset.baseline, applied.baseline);
    assert_eq!(
        reset.abandoned_action.as_ref().map(|action| action.commits),
        Some(1)
    );
    assert!(!reset.released_refs.is_empty());

    assert_eq!(
        store.automation_state(&consumer).unwrap(),
        None,
        "the forgotten consumer holds no state at all"
    );
    assert!(
        pinned_refs(&runtime).is_empty(),
        "and keeps no frozen input reachable"
    );

    // The next evaluation seeds a fresh baseline at the branch head, with a
    // recorded trigger — so a later recovery can prove its coverage contract.
    let baselined = evaluate_auto_task(&runtime, &qa, false, Utc::now()).unwrap();
    assert_eq!(baselined.reason, "baselined");
    let state = baselined.state.unwrap();
    assert_eq!(state.generation, 0);
    assert_eq!(state.baseline, applied.baseline);
    assert!(state.trigger.is_some());
    assert!(state.pending.is_empty());
    assert!(state.unresolved.is_empty());
    let preview =
        super::super::recover_auto_task(&runtime, &qa, &RecoveryRequest::default(), Utc::now())
            .unwrap();
    assert!(
        !preview
            .refusals
            .contains(&"coverage_unverifiable".to_string()),
        "{:?}",
        preview.refusals
    );
}

/// The orbit-graph shape: state written before the trigger was recorded, with
/// no frozen batch to prove its coverage contract from. Recovery refuses it,
/// which is exactly why reset may not.
#[test]
fn a_legacy_consumer_without_a_trigger_resets_even_though_recovery_refuses_it() {
    let runtime = runtime();
    let qa = definition(&runtime, "delivery-qa", CoverageClass::IntegratedQaV1);
    let consumer = consumer_key(&runtime, "auto-task", &qa.name).unwrap();
    let store = runtime.automation_store().unwrap();

    let head = git(
        &runtime.paths().repo_root,
        &["rev-parse", "refs/heads/agent-main"],
    );
    let tree = git(
        &runtime.paths().repo_root,
        &["rev-parse", "refs/heads/agent-main^{tree}"],
    );
    let observed = SourceRevision {
        commit: head.clone(),
        tree,
    };
    assert!(
        store
            .automation_initialize(&legacy(&consumer, &runtime, observed.clone()))
            .unwrap()
    );

    let refused = super::super::recover_auto_task(
        &runtime,
        &qa,
        &RecoveryRequest {
            adopt_settings: true,
            reason: "adopt the configured settings".into(),
            ..RecoveryRequest::default()
        },
        Utc::now(),
    )
    .expect_err("legacy state cannot prove which contract its debt was for");
    assert!(
        refused.to_string().contains("coverage_unverifiable"),
        "{refused}"
    );

    let preview =
        super::super::reset_auto_task(&runtime, &qa, &ResetRequest::default(), Utc::now()).unwrap();
    assert!(preview.refusals.is_empty());
    assert_eq!(preview.baseline.commit, head);

    let applied = super::super::reset_auto_task(
        &runtime,
        &qa,
        &reason("pre-0.21.0 state cannot be recovered"),
        Utc::now(),
    )
    .unwrap();
    assert!(applied.applied);
    assert_eq!(store.automation_state(&consumer).unwrap(), None);
    let records = store.automation_recoveries(&consumer, 10).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].kind(), "reset");
    assert_eq!(records[0].previous_trigger, None);

    assert_eq!(
        evaluate_auto_task(&runtime, &qa, false, Utc::now())
            .unwrap()
            .reason,
        "baselined"
    );
}

#[test]
fn a_member_consumer_is_refused_the_way_recovery_refuses_it() {
    let runtime = runtime();
    let qa = definition(&runtime, "delivery-qa", CoverageClass::IntegratedQaV1);
    let consumer = consumer_key(&runtime, "auto-task", &qa.name).unwrap();
    let head = git(
        &runtime.paths().repo_root,
        &["rev-parse", "refs/heads/agent-main"],
    );
    let tree = git(
        &runtime.paths().repo_root,
        &["rev-parse", "refs/heads/agent-main^{tree}"],
    );
    let mut state = legacy(&consumer, &runtime, SourceRevision { commit: head, tree });
    state.members = Some(MemberState::default());
    assert!(
        runtime
            .automation_store()
            .unwrap()
            .automation_initialize(&state)
            .unwrap()
    );

    let refused = super::super::reset_auto_task(
        &runtime,
        &qa,
        &reason("forget the member state"),
        Utc::now(),
    )
    .expect_err("a state-member consumer is not a delivery consumer");
    assert!(refused.to_string().contains("member_consumer"), "{refused}");
}

/// State in the shape written before the resolved trigger was recorded.
fn legacy(consumer: &str, runtime: &OrbitRuntime, at: SourceRevision) -> AutomationState {
    let source = super::super::source::Source::new(&runtime.paths().repo_root);
    AutomationState {
        members: None,
        consumer: consumer.into(),
        epoch: "pre-0.21.0-epoch".into(),
        trigger: None,
        repository: source.repository().unwrap(),
        branch: "agent-main".into(),
        generation: 0,
        baseline: at.clone(),
        observed: at.clone(),
        covered: at,
        pending_commits: vec![],
        pending: vec![],
        waived: vec![],
        excluded: vec![],
        unresolved: Default::default(),
        associations: Default::default(),
        active: None,
        stall: None,
    }
}
