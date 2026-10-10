//! A clean review batch commits as no-diff on its accepted coverage [ORB-14837].
//!
//! A delivery review leaves no change; its deliverable is the coverage
//! evidence. When its agent persists no execution summary, `git_commit`
//! derives one from evidence the automation would accept for the frozen
//! batch, and from nothing else.

use std::path::PathBuf;

use orbit_core::application::task::TaskAddParams;
use orbit_types::task::{NO_DIFF_EXPECTED_TAG, TaskComplexity};
use orbit_types::workflow::automation::CoverageEvidence;

use super::*;

/// The guard's current refusal for an unsummarized clean task.
const REFUSAL: &str = "requires a meaningful persisted execution_summary before delivery; \
                       the implementing agent recorded none and the worktree holds no \
                       uncommitted change";

/// A named edit that makes otherwise valid coverage unacceptable.
type Mismatch = (&'static str, fn(&mut CoverageEvidence));

/// A clean worktree of the checkout on its own named branch.
fn clean_worktree(fixture: &Fixture, name: &str) -> PathBuf {
    let path = fixture._temp.path().join(name);
    git(
        fixture,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            &format!("orbit/{name}"),
            &path.display().to_string(),
        ],
    );
    path
}

fn git_commit(
    runtime: &orbit_core::OrbitRuntime,
    run_id: &str,
    worktree: &Path,
) -> Result<Value, orbit_common::OrbitError> {
    orbit_engine::execute_deterministic_action(
        runtime,
        "git_commit",
        &json!({}),
        &json!({ "job_run_id": run_id, "workspace_path": worktree }),
        false,
        &Default::default(),
        None,
    )
}

fn refused(result: Result<Value, orbit_common::OrbitError>, case: &str) {
    let error = result.expect_err(case).to_string();
    assert!(error.contains(REFUSAL), "{case}: {error}");
}

#[test]
fn review_coverage_bound_to_the_batch_is_the_no_diff_evidence() {
    const TEST: &str = "delivery_remote_source::review_commit::review_coverage_bound_to_the_batch_is_the_no_diff_evidence";
    if !in_isolated_child(TEST) {
        return;
    }

    let fixture = Fixture::new();
    let (runtime, attempt, run_id) = admitted_action(&fixture, 0, REVIEW_CONSUMER);
    let action_id = attempt.action_id.clone().expect("admitted action");
    assert!(
        runtime
            .get_task(&action_id)
            .unwrap()
            .tags
            .iter()
            .any(|tag| tag == NO_DIFF_EXPECTED_TAG),
        "the shipped review template routes a clean stage to no-diff"
    );
    let worktree = clean_worktree(&fixture, "review");

    let findings = vec![
        "ORB-90001: unchecked error in fixture.txt, introduced by ORB-90000".to_string(),
        "ORB-90002: duplicate of an open finding, not refiled".to_string(),
    ];
    let mut complete = complete_evidence(&attempt);
    complete.findings = findings.clone();

    // Evidence the automation would refuse derives nothing, so the guard
    // refuses as it does for any unsummarized clean task.
    let mismatches: [Mismatch; 5] = [
        ("incomplete examination", |evidence| {
            evidence.examination_complete = false;
        }),
        ("another batch", |evidence| {
            evidence.batch_id = "another-batch".into();
        }),
        ("another epoch", |evidence| {
            evidence.epoch = "another-epoch".into();
        }),
        ("another input", |evidence| {
            evidence.input_digest = "another-input".into();
        }),
        ("unexamined delivery", |evidence| {
            evidence.delivery_examinations[0].examined_paths.clear();
        }),
    ];
    for (case, mismatch) in mismatches {
        let mut evidence = complete.clone();
        mismatch(&mut evidence);
        put_coverage(&fixture, &runtime, &action_id, &run_id, &evidence);
        refused(git_commit(&runtime, &run_id, &worktree), case);
        assert_eq!(
            runtime.get_task(&action_id).unwrap().execution_summary,
            "",
            "{case}: refused evidence must persist no summary"
        );
    }

    // An ordinary task in the same workspace has no coverage to stand on.
    let plain = runtime
        .add_task(TaskAddParams {
            title: "Ordinary implementation".into(),
            complexity: TaskComplexity::Low,
            ..Default::default()
        })
        .unwrap();
    let plain_run = RuntimeHost::insert_job_run(
        &runtime,
        "task-pr-pipeline",
        1,
        Utc::now(),
        Some(json!({ "task_id": plain.id })),
        None,
    )
    .unwrap();
    RuntimeHost::apply_task_automation_update(
        &runtime,
        &plain.id,
        TaskAutomationUpdate {
            job_run_id: Some(plain_run.run_id.clone()),
            ..TaskAutomationUpdate::default()
        },
    )
    .unwrap();
    refused(
        git_commit(
            &runtime,
            &plain_run.run_id,
            &clean_worktree(&fixture, "plain"),
        ),
        "ordinary task",
    );

    put_coverage(&fixture, &runtime, &action_id, &run_id, &complete);
    let output = git_commit(&runtime, &run_id, &worktree).expect("accepted coverage commits");
    assert_eq!(output["decision"], "skipped_no_diff_expected", "{output}");
    assert_eq!(output["committed"], false, "{output}");

    let summary = runtime.get_task(&action_id).unwrap().execution_summary;
    let range = format!(
        "{}..{}",
        attempt.batch.from_exclusive.commit, attempt.batch.through_inclusive.commit
    );
    assert!(summary.contains(&range), "range: {summary}");
    assert!(
        summary.contains(&format!(
            "Examined {} commit(s) and {} delivery(ies)",
            attempt.batch.commits.len(),
            attempt.batch.deliveries.len()
        )),
        "counts: {summary}"
    );
    for finding in &findings {
        assert!(summary.contains(finding.as_str()), "{finding}: {summary}");
    }
}

/// A review whose agent persisted no execution summary still commits on a
/// summary Orbit derives from its evidence, but that summary does not pay
/// for the batch: settlement records the typed reason, spends the retry and
/// leaves the obligation owed [ORB-15186].
#[test]
fn a_derived_summary_leaves_the_batch_owed() {
    const TEST: &str =
        "delivery_remote_source::review_commit::a_derived_summary_leaves_the_batch_owed";
    if !in_isolated_child(TEST) {
        return;
    }

    let fixture = Fixture::new();
    let (runtime, attempt, run_id) = admitted_action(&fixture, 1, REVIEW_CONSUMER);
    let action_id = attempt.action_id.clone().expect("admitted action");
    let before = named_consumer_state(&runtime, REVIEW_CONSUMER);
    put_coverage(
        &fixture,
        &runtime,
        &action_id,
        &run_id,
        &complete_evidence(&attempt),
    );
    let worktree = clean_worktree(&fixture, "review");
    git_commit(&runtime, &run_id, &worktree).expect("derived summary commits");
    let summary = runtime.get_task(&action_id).unwrap().execution_summary;
    assert!(
        summary.contains("Delivery verdicts (1):"),
        "the derived summary describes the evidence: {summary}"
    );

    // An open review may still write its own summary, so nothing settles.
    with_pull_lookup(&fixture, || {
        let definition = runtime.auto_task_show(REVIEW_CONSUMER).unwrap().unwrap();
        evaluate_auto_task(&runtime, &definition, false, Utc::now())
    })
    .expect("evaluate the open review");
    assert_eq!(
        named_consumer_state(&runtime, REVIEW_CONSUMER).active,
        before.active
    );

    fixture.json(&[
        "task", "update", &action_id, "--status", "done", "--force", "--json",
    ]);
    with_pull_lookup(&fixture, || {
        let definition = runtime.auto_task_show(REVIEW_CONSUMER).unwrap().unwrap();
        evaluate_auto_task(&runtime, &definition, false, Utc::now())
    })
    .expect("evaluate the closed review");
    let state = named_consumer_state(&runtime, REVIEW_CONSUMER);
    assert_eq!(state.covered, before.covered, "{state:#?}");
    let active = state.active.as_ref().expect("the batch stays owed");
    assert_eq!(active.attempt, 2, "{active:#?}");
    assert_eq!(
        active.reason.as_deref(),
        Some("review_closed_without_execution_summary")
    );
    assert!(
        runtime
            .automation_store()
            .unwrap()
            .automation_receipts(&state.consumer, 10)
            .unwrap()
            .is_empty()
    );

    let shown = fixture.json(&["auto-task", "show", REVIEW_CONSUMER, "--json"]);
    assert_eq!(
        shown["automation"]["state"]["active"]["reason"], "review_closed_without_execution_summary",
        "{shown}"
    );
}

/// The derived-summary history event outlives the agent's own replacement, so
/// the current summary text, not the event, decides who wrote it: a review that
/// replaces Orbit's summary settles the batch without spending a retry
/// [ORB-15255].
#[test]
fn an_agent_replacement_of_a_derived_summary_settles_the_batch() {
    const TEST: &str = "delivery_remote_source::review_commit::an_agent_replacement_of_a_derived_summary_settles_the_batch";
    if !in_isolated_child(TEST) {
        return;
    }

    let fixture = Fixture::new();
    let (runtime, attempt, run_id) = admitted_action(&fixture, 1, REVIEW_CONSUMER);
    let action_id = attempt.action_id.clone().expect("admitted action");
    put_coverage(
        &fixture,
        &runtime,
        &action_id,
        &run_id,
        &complete_evidence(&attempt),
    );
    let worktree = clean_worktree(&fixture, "review");
    git_commit(&runtime, &run_id, &worktree).expect("derived summary commits");
    assert!(
        runtime
            .get_task(&action_id)
            .unwrap()
            .execution_summary
            .starts_with(orbit_types::task::DERIVED_EXECUTION_SUMMARY_PREFIX),
        "the commit step derived the summary"
    );

    fixture.json(&[
        "task",
        "update",
        &action_id,
        "--execution-summary",
        "Examined the frozen delivery and its changed paths; no defects found.",
        "--json",
    ]);
    fixture.json(&[
        "task", "update", &action_id, "--status", "done", "--force", "--json",
    ]);
    with_pull_lookup(&fixture, || {
        let definition = runtime.auto_task_show(REVIEW_CONSUMER).unwrap().unwrap();
        evaluate_auto_task(&runtime, &definition, false, Utc::now())
    })
    .expect("evaluate the closed review");

    let state = named_consumer_state(&runtime, REVIEW_CONSUMER);
    assert_eq!(state.covered, attempt.batch.through_inclusive, "{state:#?}");
    assert!(state.active.is_none(), "no retry is spent: {state:#?}");
    let receipts = runtime
        .automation_store()
        .unwrap()
        .automation_receipts(&state.consumer, 10)
        .unwrap();
    assert_eq!(receipts.len(), 1, "{receipts:#?}");
    assert!(
        runtime
            .get_task_history(&action_id)
            .unwrap()
            .iter()
            .any(|entry| entry.event == orbit_types::task::EXECUTION_SUMMARY_DERIVED_EVENT),
        "the derivation stays in the task's history"
    );
}
