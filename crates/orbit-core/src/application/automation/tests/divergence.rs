//! Real Git rewrites of a watched branch, against a live delivery consumer.
//!
//! Both shapes of the ORB-12346 incident are exercised end to end: the rebase
//! that preserved content, which the evaluator has to prove and reconcile by
//! itself, and the rewrite that dropped a commit, which it must refuse to
//! reconcile and escalate instead.

use super::consumer::{commit, definition, git, runtime};
use crate::OrbitRuntime;
use chrono::{Duration, Utc};
use orbit_store::contracts::{FrictionListFilter, StoredFrictionRecord};
use orbit_types::workflow::automation::CoverageClass;
use orbit_types::workflow::automation::recovery::{RecoveryRequest, ResetRequest, SYSTEM_ACTOR};

/// A fixture repository whose commits carry a stable `.orbit` tree, which is
/// half of the replay proof's content signature.
fn diverging_runtime() -> OrbitRuntime {
    let runtime = runtime();
    let root = &runtime.paths().repo_root;
    std::fs::create_dir_all(root.join(".orbit")).unwrap();
    std::fs::write(root.join(".orbit/stable.toml"), "version = 1\n").unwrap();
    git(root, &["add", ".orbit/stable.toml"]);
    git(root, &["commit", "-m", "pin the orbit tree"]);

    runtime
}

fn frictions(runtime: &OrbitRuntime) -> Vec<StoredFrictionRecord> {
    crate::runtime::friction::store_for(runtime)
        .unwrap()
        .list(&FrictionListFilter::default())
        .unwrap()
}

fn evaluate(
    runtime: &OrbitRuntime,
    definition: &orbit_types::workflow::AutoTaskDefinition,
) -> String {
    super::super::evaluate_auto_task(runtime, definition, false, Utc::now())
        .unwrap()
        .reason
}

#[test]
fn an_amended_observed_commit_is_replayed_automatically_and_reported_once() {
    let runtime = diverging_runtime();
    let root = runtime.paths().repo_root.clone();
    let qa = definition(&runtime, "delivery-qa", CoverageClass::IntegratedQaV1);
    let review = definition(
        &runtime,
        "delivery-code-review",
        CoverageClass::LandedCodeReviewV1,
    );
    assert_eq!(evaluate(&runtime, &qa), "baselined");
    assert_eq!(evaluate(&runtime, &review), "baselined");

    let observed = commit(&root, "unexamined change");
    assert_eq!(evaluate(&runtime, &qa), "evidence_unavailable");
    assert_eq!(evaluate(&runtime, &review), "evidence_unavailable");
    let consumer = super::super::consumer_key(&runtime, "auto-task", &qa.name).unwrap();
    let store = runtime.automation_store().unwrap();
    assert_eq!(
        store
            .automation_state(&consumer)
            .unwrap()
            .unwrap()
            .observed
            .commit,
        observed
    );

    // A `pull --rebase` that only rewrote the commit message: the same tree,
    // the same parent-relative patch, a new commit id.
    git(
        &root,
        &["commit", "--amend", "-m", "unexamined change (rebased)"],
    );
    let canonical = git(&root, &["rev-parse", "HEAD"]);
    assert_ne!(canonical, observed);

    assert_eq!(
        evaluate(&runtime, &qa),
        "history_replayed",
        "one tick proves and applies the replay without an operator"
    );

    let state = store.automation_state(&consumer).unwrap().unwrap();
    assert_eq!(state.observed.commit, canonical);
    assert_eq!(state.pending_commits, vec![canonical.clone()]);
    assert!(state.unresolved.contains_key(&canonical));
    assert_eq!(state.stall, None, "a proven replay leaves no stall behind");

    let recoveries = store.automation_recoveries(&consumer, 10).unwrap();
    assert_eq!(recoveries.len(), 1);
    let record = &recoveries[0];
    assert_eq!(record.kind(), "replay_history");
    assert_eq!(record.by, SYSTEM_ACTOR, "no human is credited for this");
    let replay = record.replayed_history.as_ref().unwrap();
    assert_eq!(replay.old_observed.commit, observed);
    assert_eq!(replay.new_observed.commit, canonical);
    assert_eq!(replay.mappings.len(), 1);
    assert!(replay.added_obligations.is_empty());

    let filed = frictions(&runtime);
    assert_eq!(filed.len(), 1, "exactly one friction for the rewrite");
    assert!(
        filed[0]
            .record
            .tags
            .contains(&"history-diverged".to_string())
    );
    assert!(
        filed[0].record.body.contains(&observed) && filed[0].record.body.contains(&canonical),
        "the record names the rewrite it reports"
    );
    assert_eq!(
        record.friction_id, None,
        "the replay is reported only after it commits, so it answers no filed friction"
    );

    // The next tick is ordinary observation again, and the sibling consumer
    // watching the same branch reuses the same record for the same rewrite.
    assert_eq!(evaluate(&runtime, &qa), "evidence_unavailable");
    assert_eq!(evaluate(&runtime, &review), "history_replayed");
    assert_eq!(evaluate(&runtime, &review), "evidence_unavailable");
    assert_eq!(
        frictions(&runtime).len(),
        1,
        "repeated ticks and the sibling consumer share one divergence record"
    );
}

#[test]
fn a_dropped_observed_commit_stalls_the_consumer_instead_of_erroring_every_tick() {
    let runtime = diverging_runtime();
    let root = runtime.paths().repo_root.clone();
    let qa = definition(&runtime, "delivery-qa", CoverageClass::IntegratedQaV1);
    let review = definition(
        &runtime,
        "delivery-code-review",
        CoverageClass::LandedCodeReviewV1,
    );
    assert_eq!(evaluate(&runtime, &qa), "baselined");
    assert_eq!(evaluate(&runtime, &review), "baselined");

    let dropped = commit(&root, "unexamined change");
    assert_eq!(evaluate(&runtime, &qa), "evidence_unavailable");
    assert_eq!(evaluate(&runtime, &review), "evidence_unavailable");

    // The observed commit is gone from the branch: nothing maps its content
    // onto the new head, so the debt it carried cannot be reconciled.
    git(&root, &["reset", "--hard", "HEAD~1"]);

    assert_eq!(
        evaluate(&runtime, &qa),
        "stalled: history_diverged",
        "the tick reports the stall instead of a per-tick execution error"
    );

    let consumer = super::super::consumer_key(&runtime, "auto-task", &qa.name).unwrap();
    let store = runtime.automation_store().unwrap();
    let stall = store
        .automation_state(&consumer)
        .unwrap()
        .unwrap()
        .stall
        .unwrap();
    assert_eq!(stall.reason, "history_diverged");
    let divergence = stall.divergence.as_ref().unwrap();
    assert_eq!(divergence.observed.commit, dropped);
    assert_eq!(divergence.refusal, "history_mapping_ambiguous");
    assert!(
        divergence
            .obligations
            .iter()
            .any(|obligation| obligation.contains(&dropped)),
        "{:?}",
        divergence.obligations
    );

    let filed = frictions(&runtime);
    assert_eq!(filed.len(), 1);
    assert!(
        filed[0]
            .record
            .tags
            .contains(&"history-diverged".to_string())
    );
    assert!(
        filed[0].record.body.contains(&dropped),
        "the record names the obligations the proof could not map"
    );
    assert!(filed[0].record.body.contains("orbit auto-task reset"));
    assert_eq!(stall.friction_id.as_ref(), Some(&filed[0].record.id));

    // The stall classifier reports the real reason, not `not_stalled`.
    let preview =
        super::super::recover_auto_task(&runtime, &qa, &RecoveryRequest::default(), Utc::now())
            .unwrap();
    assert_eq!(preview.reason, "history_diverged");

    // And `orbit doctor` reads the same marker.
    let stalled = super::super::stalled_consumers(&runtime).unwrap();
    assert_eq!(stalled.len(), 1);
    assert_eq!(stalled[0].definition(), "delivery-qa");
    assert_eq!(stalled[0].stall.reason, "history_diverged");

    // Ten further ticks: one stalled line each, no new friction, no error.
    for _ in 0..10 {
        assert_eq!(evaluate(&runtime, &qa), "stalled: history_diverged");
    }
    assert_eq!(evaluate(&runtime, &review), "stalled: history_diverged");
    assert_eq!(
        frictions(&runtime).len(),
        1,
        "repeated ticks and the sibling consumer never file a second record"
    );

    // The operator's way out: reset forgets the unreachable debt and the
    // consumer re-baselines at the branch head on the next tick.
    let head = git(&root, &["rev-parse", "refs/heads/agent-main"]);
    super::super::reset_auto_task(
        &runtime,
        &qa,
        &ResetRequest {
            reason: "agent-main was rewritten past the observed commit".into(),
            force: false,
        },
        Utc::now() + Duration::minutes(1),
    )
    .unwrap();
    assert_eq!(evaluate(&runtime, &qa), "baselined");
    assert_eq!(
        store
            .automation_state(&consumer)
            .unwrap()
            .unwrap()
            .baseline
            .commit,
        head
    );
    let records = store.automation_recoveries(&consumer, 10).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].kind(), "reset");
    assert_eq!(
        records[0].friction_id.as_ref(),
        Some(&filed[0].record.id),
        "the audit record links the friction the stall filed"
    );
    assert!(
        super::super::stalled_consumers(&runtime)
            .unwrap()
            .iter()
            .all(|stalled| stalled.definition() != "delivery-qa")
    );
}

#[test]
fn a_stall_clears_itself_only_when_the_observed_commit_is_reachable_again() {
    let runtime = diverging_runtime();
    let root = runtime.paths().repo_root.clone();
    let qa = definition(&runtime, "delivery-qa", CoverageClass::IntegratedQaV1);
    assert_eq!(evaluate(&runtime, &qa), "baselined");

    let observed = commit(&root, "unexamined change");
    assert_eq!(evaluate(&runtime, &qa), "evidence_unavailable");

    git(&root, &["reset", "--hard", "HEAD~1"]);
    assert_eq!(evaluate(&runtime, &qa), "stalled: history_diverged");

    // Restoring the branch makes the recorded divergence provably untrue, so
    // the consumer resumes without an audited recovery: no debt moved.
    git(&root, &["reset", "--hard", &observed]);
    assert_eq!(evaluate(&runtime, &qa), "evidence_unavailable");

    let consumer = super::super::consumer_key(&runtime, "auto-task", &qa.name).unwrap();
    let state = runtime
        .automation_store()
        .unwrap()
        .automation_state(&consumer)
        .unwrap()
        .unwrap();
    assert_eq!(state.stall, None);
    assert_eq!(state.observed.commit, observed);
    assert_eq!(
        frictions(&runtime).len(),
        1,
        "the record of the rewrite stays; it was real"
    );
}
