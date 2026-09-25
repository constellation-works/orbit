//! Judging the reviewer's claims: repairs, scope, validation evidence, and budget.

use std::fs;

use chrono::{Duration, Utc};
use orbit_automation::review::{combined_task_meaning_digest, task_meaning_digest};
use orbit_store::contracts::ReviewReserveRequest;
use orbit_types::workflow::automation::SourceRevision;
use orbit_types::workflow::{
    REVIEW_CONTRACT_VERSION, ReviewAttemptState, ReviewBudget, ReviewReservation, ReviewVerdict,
    ValidationOutcome, ValidationRole,
};
use serde_json::json;

use crate::application::automation::source::Source;
use crate::application::review::tests::{GATED_CONFIG, git, report, validation, write_report};
use crate::application::review::{exclusions, lineage_key};
use crate::application::task::TaskUpdateParams;

use super::support::{gated_fixture, land_squash, page_for};

#[test]
fn reviewer_repairs_land_as_separate_attributed_commits_and_bind_the_final_tree() {
    let gated = gated_fixture(GATED_CONFIG);
    let admission = gated.admit().expect("admit");
    let attempt_id = admission["attempt_id"].as_str().expect("attempt id");

    // The reviewer edits the worktree and reports the repair; it never commits.
    fs::write(
        gated.fixture.repo.join("src.txt"),
        "implementation target\nimplemented\nrepaired\n",
    )
    .expect("repair");
    write_report(
        &gated.fixture.runtime,
        &gated.task_id,
        &report(attempt_id, ReviewVerdict::PassedWithRepairs, true),
    );
    let settled = gated.settle(&admission).expect("settle");
    assert_eq!(settled["gate"], "passed");
    assert_eq!(settled["verdict"], "passed_with_repairs");
    assert_eq!(
        settled["assurance"],
        "independent_review_with_self_authored_repairs"
    );

    let head = gated.head();
    assert_ne!(head, gated.implementation_sha);
    assert_eq!(settled["reviewed_head_sha"], head);
    assert_eq!(
        gated.author_of("HEAD~1"),
        "orbit[impl-model] <agent@orbit.invalid>"
    );
    assert_eq!(
        gated.author_of("HEAD"),
        "codex-reviewer <codex-reviewer@orbit.local>"
    );
    let message = git(&gated.fixture.repo, &["log", "-1", "--format=%B", "HEAD"]);
    assert!(message.contains(&format!("Orbit-Review-Attempt: {attempt_id}")));
    assert!(message.contains("Findings: F1"));

    let certificate = gated.certificate();
    assert_eq!(
        certificate.reviewed_candidate.commit,
        gated.implementation_sha
    );
    assert_eq!(certificate.final_candidate.commit, head);
    assert_eq!(certificate.repair_commits.len(), 1);
    assert_eq!(certificate.consumed.repair_cycles, 1);
    let ledger = gated
        .fixture
        .runtime
        .review_store()
        .expect("store")
        .review_ledger(
            &gated.fixture.runtime.workspace_id().expect("ws"),
            &certificate.lineage_key,
        )
        .expect("ledger")
        .expect("ledger present");
    assert_eq!(
        ledger.attempts[0].state,
        ReviewAttemptState::Settled {
            verdict: ReviewVerdict::PassedWithRepairs
        }
    );

    // Replaying settlement after the fact reconciles the certificate: no new
    // commit, no new attempt.
    let replay = gated.settle(&admission).expect("replay");
    assert_eq!(replay["reviewed_head_sha"], head);
    assert_eq!(gated.head(), head);
    assert_eq!(
        gated
            .fixture
            .runtime
            .review_store()
            .expect("store")
            .review_ledger(
                &gated.fixture.runtime.workspace_id().expect("ws"),
                &certificate.lineage_key
            )
            .expect("ledger")
            .expect("present")
            .attempts
            .len(),
        1
    );
}

#[test]
fn a_declared_out_of_selector_repair_widens_context_files_and_passes() {
    let gated = gated_fixture(GATED_CONFIG);
    let admission = gated.admit().expect("admit");
    let attempt_id = admission["attempt_id"].as_str().expect("attempt id");

    fs::write(
        gated.fixture.repo.join("README.md"),
        "coupled repair of derived artifact\n",
    )
    .expect("repair");
    let mut claim = report(attempt_id, ReviewVerdict::PassedWithRepairs, true);
    claim.findings[0].paths = vec!["README.md".to_string()];
    write_report(&gated.fixture.runtime, &gated.task_id, &claim);

    let settled = gated.settle(&admission).expect("settle");
    assert_eq!(settled["gate"], "passed");
    assert_eq!(settled["verdict"], "passed_with_repairs");
    assert!(
        gated
            .context_files()
            .iter()
            .any(|selector| selector == "file:README.md"),
        "the gate widens the declared repair path: {:?}",
        gated.context_files()
    );

    let certificate = gated.certificate();
    assert_eq!(
        certificate.selectors_widened,
        vec!["file:README.md".to_string()]
    );
    let comments = gated
        .fixture
        .runtime
        .get_task_comments(&gated.task_id)
        .expect("comments");
    assert!(
        comments
            .last()
            .is_some_and(|comment| comment.message.contains("file:README.md")),
        "the verdict comment mentions the widened selector: {:?}",
        comments.last().map(|comment| &comment.message)
    );
}

#[test]
fn a_repair_in_a_canonical_symbol_selector_file_passes_without_a_redundant_file_selector() {
    let gated = gated_fixture(GATED_CONFIG);
    gated.rescope(&["symbol:src.txt#run:function"]);
    let admission = gated.admit().expect("admit");
    let attempt_id = admission["attempt_id"].as_str().expect("attempt id");

    fs::write(
        gated.fixture.repo.join("src.txt"),
        "implementation target\nimplemented\nrepaired\n",
    )
    .expect("repair");
    write_report(
        &gated.fixture.runtime,
        &gated.task_id,
        &report(attempt_id, ReviewVerdict::PassedWithRepairs, true),
    );
    let settled = gated.settle(&admission).expect("settle");
    assert_eq!(settled["gate"], "passed");
    assert_eq!(settled["verdict"], "passed_with_repairs");
    assert_eq!(
        gated.context_files(),
        vec!["symbol:src.txt#run:function".to_string()],
        "a symbol selector already authorizes its backing file"
    );
}

#[test]
fn qualified_rust_symbol_selectors_share_the_file_anchor_and_unrelated_paths_still_downgrade() {
    let gated = gated_fixture(GATED_CONFIG);
    gated.rescope(&["symbol:src.txt#orbit_core::run:function"]);
    let admission = gated.admit().expect("admit");
    let attempt_id = admission["attempt_id"].as_str().expect("attempt id");

    fs::write(
        gated.fixture.repo.join("src.txt"),
        "implementation target\nimplemented\nrepaired\n",
    )
    .expect("repair");
    write_report(
        &gated.fixture.runtime,
        &gated.task_id,
        &report(attempt_id, ReviewVerdict::PassedWithRepairs, true),
    );
    let settled = gated.settle(&admission).expect("settle");
    assert_eq!(settled["gate"], "passed");
    assert_eq!(settled["verdict"], "passed_with_repairs");

    let unrelated = gated_fixture(GATED_CONFIG);
    unrelated.rescope(&["symbol:src.txt#orbit_core::run:function"]);
    let admission = unrelated.admit().expect("admit");
    let attempt_id = admission["attempt_id"].as_str().expect("attempt id");
    fs::write(
        unrelated.fixture.repo.join("README.md"),
        "out of scope repair\n",
    )
    .expect("edit");
    write_report(
        &unrelated.fixture.runtime,
        &unrelated.task_id,
        &report(attempt_id, ReviewVerdict::PassedWithRepairs, true),
    );
    let error = unrelated.settle(&admission).expect_err("out of scope");
    let message = error.to_string();
    assert!(message.contains("repair_out_of_scope"), "{message}");
    assert!(
        message.contains("README.md was changed but named by no finding"),
        "{message}"
    );
    assert_eq!(unrelated.certificate().verdict, ReviewVerdict::Incomplete);
    assert_eq!(
        unrelated.context_files(),
        vec!["symbol:src.txt#orbit_core::run:function".to_string()],
        "an undeclared repair must not widen selectors"
    );
}

#[test]
fn inconsistent_claims_and_denied_validation_are_downgraded_honestly() {
    // Claims "no repairs" but changed the tree.
    let gated = gated_fixture(GATED_CONFIG);
    let admission = gated.admit().expect("admit");
    let attempt_id = admission["attempt_id"]
        .as_str()
        .expect("attempt id")
        .to_string();
    fs::write(gated.fixture.repo.join("src.txt"), "changed by reviewer\n").expect("edit");
    write_report(
        &gated.fixture.runtime,
        &gated.task_id,
        &report(&attempt_id, ReviewVerdict::PassedWithoutRepairs, false),
    );
    let error = gated.settle(&admission).expect_err("inconsistent");
    assert!(
        error.to_string().contains("verdict_inconsistent"),
        "{error}"
    );
    // The partial work is still preserved under the reviewer's identity.
    assert_eq!(
        gated.author_of("HEAD"),
        "codex-reviewer <codex-reviewer@orbit.local>"
    );

    // Denied validation on a second lineage.
    let denied = gated_fixture(GATED_CONFIG);
    let admission = denied.admit().expect("admit");
    let attempt_id = admission["attempt_id"]
        .as_str()
        .expect("attempt id")
        .to_string();
    let mut claim = report(&attempt_id, ReviewVerdict::PassedWithoutRepairs, false);
    claim.validation[0].outcome = orbit_types::workflow::ValidationOutcome::Denied;
    write_report(&denied.fixture.runtime, &denied.task_id, &claim);
    let error = denied.settle(&admission).expect_err("denied validation");
    assert!(
        error.to_string().contains("validation_unavailable"),
        "{error}"
    );
    assert_eq!(denied.certificate().verdict, ReviewVerdict::Incomplete);
}

#[test]
fn a_negative_control_and_a_scope_exclusion_pass_alongside_required_checks() {
    // ORB-11511: the reviewer honestly recorded the superseded assertion as
    // failed (which is what proves the regression) and the unauthorized
    // deployment as not run. Neither is a required candidate check, so the
    // pass is correct and the record stays auditable.
    let gated = gated_fixture(GATED_CONFIG);
    let admission = gated.admit().expect("admit");
    let attempt_id = admission["attempt_id"].as_str().expect("attempt id");

    let mut claim = report(attempt_id, ReviewVerdict::PassedWithoutRepairs, false);
    claim.validation = vec![
        validation(
            "make ci-fast",
            ValidationOutcome::Passed,
            ValidationRole::Required,
            None,
        ),
        validation(
            "make ci-lint",
            ValidationOutcome::Passed,
            ValidationRole::Required,
            None,
        ),
        validation(
            "grep -q 'Strict-Transport-Security' old-config",
            ValidationOutcome::Failed,
            ValidationRole::ExpectedFailure,
            Some("negative control: the superseded assertion must no longer hold"),
        ),
        validation(
            "wrangler deploy",
            ValidationOutcome::NotRun,
            ValidationRole::Excluded,
            Some("live deployment is explicitly outside the authorized scope"),
        ),
    ];
    write_report(&gated.fixture.runtime, &gated.task_id, &claim);

    let settled = gated.settle(&admission).expect("settle");
    assert_eq!(settled["gate"], "passed");
    assert_eq!(settled["verdict"], "passed_without_repairs");

    let certificate = gated.certificate();
    assert!(certificate.validation_complete);
    assert_eq!(certificate.escalation, None);
    assert_eq!(
        certificate.validation, claim.validation,
        "every raw observation and its classification survive into the certificate"
    );

    let comments = gated
        .fixture
        .runtime
        .get_task_comments(&gated.task_id)
        .expect("comments");
    assert!(
        comments.last().is_some_and(|comment| comment
            .message
            .contains("4 record(s) [2 required, 1 expected_failure, 1 excluded]")),
        "the classification breakdown is disclosed on the task"
    );

    // The deterministic consumer reads the same contract: this certificate
    // is spendable coverage for the landing that reproduces it.
    let source = Source::new(&gated.fixture.repo);
    let repository = source.repository().expect("repository");
    let delivery = land_squash(&gated, &repository);
    let (state, mut page) = page_for(&delivery);
    exclusions(&gated.fixture.runtime, &source, &state, &mut page).expect("exclusions");
    assert!(
        page.exclusions.contains_key(&delivery.key),
        "a controlled and scope-excluded validation set still covers its landing"
    );
}

#[test]
fn classifications_that_contradict_their_outcome_or_explain_nothing_are_refused() {
    // A negative control that passed disproves what it was recorded for.
    let contradicted = gated_fixture(GATED_CONFIG);
    let admission = contradicted.admit().expect("admit");
    let attempt_id = admission["attempt_id"].as_str().expect("attempt id");
    let mut claim = report(attempt_id, ReviewVerdict::PassedWithoutRepairs, false);
    claim.validation.push(validation(
        "cargo test old_assertion",
        ValidationOutcome::Passed,
        ValidationRole::ExpectedFailure,
        Some("the pre-fix reproduction must fail"),
    ));
    write_report(&contradicted.fixture.runtime, &contradicted.task_id, &claim);
    let error = contradicted.settle(&admission).expect_err("contradicted");
    assert!(
        error.to_string().contains("validation_contradicted"),
        "{error}"
    );
    assert_eq!(
        contradicted.certificate().verdict,
        ReviewVerdict::Incomplete
    );

    // An exclusion with nothing explaining it is not a scope decision.
    let unexplained = gated_fixture(GATED_CONFIG);
    let admission = unexplained.admit().expect("admit");
    let attempt_id = admission["attempt_id"].as_str().expect("attempt id");
    let mut claim = report(attempt_id, ReviewVerdict::PassedWithoutRepairs, false);
    claim.validation.push(validation(
        "wrangler deploy",
        ValidationOutcome::NotRun,
        ValidationRole::Excluded,
        None,
    ));
    write_report(&unexplained.fixture.runtime, &unexplained.task_id, &claim);
    let error = unexplained.settle(&admission).expect_err("unexplained");
    assert!(
        error.to_string().contains("validation_unexplained"),
        "{error}"
    );
}

#[test]
fn a_legacy_report_without_classifications_keeps_its_conservative_reading() {
    // Evidence written before the contract names no role, so every command
    // it lists is still a required check.
    let gated = gated_fixture(GATED_CONFIG);
    let admission = gated.admit().expect("admit");
    let attempt_id = admission["attempt_id"].as_str().expect("attempt id");
    let legacy = json!({
        "schema_version": REVIEW_CONTRACT_VERSION,
        "attempt_id": attempt_id,
        "verdict": "passed_without_repairs",
        "summary": "Checked the change against the criteria.",
        "findings": [],
        "validation": [
            {"command": "make ci-fast", "outcome": "passed"},
            {"command": "deploy to production", "outcome": "not_run"},
        ],
    });
    write_report(
        &gated.fixture.runtime,
        &gated.task_id,
        &serde_json::from_value(legacy).expect("legacy report"),
    );
    let error = gated.settle(&admission).expect_err("conservative");
    assert!(
        error
            .to_string()
            .contains("required check `deploy to production` is not_run"),
        "{error}"
    );
}

#[test]
fn a_superseded_test_is_not_replaced_by_an_unrelated_formatter() {
    let gated = gated_fixture(GATED_CONFIG);
    let admission = gated.admit().expect("admit");
    let attempt_id = admission["attempt_id"].as_str().expect("attempt id");

    let mut claim = report(attempt_id, ReviewVerdict::PassedWithoutRepairs, false);
    claim.validation = vec![
        validation(
            "cargo test",
            ValidationOutcome::Failed,
            ValidationRole::Superseded,
            Some("rerun after repair"),
        ),
        validation(
            "cargo fmt --check",
            ValidationOutcome::Passed,
            ValidationRole::Required,
            None,
        ),
    ];
    write_report(&gated.fixture.runtime, &gated.task_id, &claim);

    let error = gated.settle(&admission).expect_err("unrelated formatter");
    assert!(
        error
            .to_string()
            .contains("superseded attempt `cargo test` is followed by no related required check"),
        "{error}"
    );
    let certificate = gated.certificate();
    assert_eq!(certificate.verdict, ReviewVerdict::Incomplete);
    assert!(!certificate.validation_complete);
    assert_eq!(
        certificate.validation, claim.validation,
        "the failed test observation is preserved on the incomplete certificate"
    );
}

#[test]
fn a_superseded_attempt_is_replaced_by_a_related_corrected_rerun() {
    let gated = gated_fixture(GATED_CONFIG);
    let admission = gated.admit().expect("admit");
    let attempt_id = admission["attempt_id"].as_str().expect("attempt id");

    let mut claim = report(attempt_id, ReviewVerdict::PassedWithoutRepairs, false);
    claim.validation = vec![
        validation(
            "cargo test --package orbit-core",
            ValidationOutcome::Failed,
            ValidationRole::Superseded,
            Some("sandbox allowlist leak; rerun below in a corrected environment"),
        ),
        validation(
            "ORBIT_TEST_ALLOWLIST=1 cargo test --package orbit-core",
            ValidationOutcome::Passed,
            ValidationRole::Required,
            None,
        ),
    ];
    claim.validation[0].check = Some("orbit-core-tests".into());
    claim.validation[1].check = Some("orbit-core-tests".into());
    write_report(&gated.fixture.runtime, &gated.task_id, &claim);

    let settled = gated.settle(&admission).expect("related rerun settles");
    assert_eq!(settled["gate"], "passed");
    assert_eq!(settled["verdict"], "passed_without_repairs");

    let certificate = gated.certificate();
    assert!(certificate.validation_complete);
    assert_eq!(certificate.escalation, None);
    assert_eq!(
        certificate.validation, claim.validation,
        "the failed observation and the corrected rerun both survive into the certificate"
    );

    let source = Source::new(&gated.fixture.repo);
    let repository = source.repository().expect("repository");
    let delivery = land_squash(&gated, &repository);
    let (state, mut page) = page_for(&delivery);
    exclusions(&gated.fixture.runtime, &source, &state, &mut page).expect("exclusions");
    assert!(
        page.exclusions.contains_key(&delivery.key),
        "a related corrected rerun still covers its landing"
    );
}

#[test]
fn an_over_budget_pass_cannot_issue_a_certificate() {
    let gated = gated_fixture(
        "[crews.reviewers]\nmodel = \"review-model\"\nprovider = \"codex\"\nbackend = \"cli\"\n[crews.implementer]\nmodel = \"impl-model\"\nprovider = \"codex\"\nbackend = \"cli\"\n[workflow]\ndefault_crew = \"implementer\"\n[operation]\nreview_policy = \"before-pr\"\nreview_crew = \"reviewers\"\nreview_minutes = 1\n",
    );
    let task = gated
        .fixture
        .runtime
        .get_task(&gated.task_id)
        .expect("task");
    let digest = task_meaning_digest(&task).expect("digest");
    let combined =
        combined_task_meaning_digest(&[(task.id.to_string(), digest)]).expect("combined");
    let workspace_id = gated.fixture.runtime.workspace_id().expect("workspace");
    let lineage = lineage_key(&workspace_id, std::slice::from_ref(&gated.task_id), "main");
    let started = Utc::now() - Duration::minutes(2);
    let candidate = SourceRevision {
        commit: gated.implementation_sha.clone(),
        tree: git(&gated.fixture.repo, &["rev-parse", "HEAD^{tree}"]),
    };
    let store = gated.fixture.runtime.review_store().expect("store");
    let ReviewReservation::Reserved { attempt } = store
        .review_reserve(
            &workspace_id,
            &ReviewReserveRequest {
                lineage_key: &lineage,
                task_ids: std::slice::from_ref(&gated.task_id),
                run_id: &gated.run_id,
                task_meaning_digest: &combined,
                candidate: &candidate,
                budget: ReviewBudget {
                    reviewer_starts: 2,
                    repair_cycles: 2,
                    minutes: 1,
                },
                now: started,
            },
        )
        .expect("pre-reserve")
        .0
    else {
        panic!("pre-reserve a start two minutes ago");
    };

    let admission = gated.admit().expect("resume the overdue attempt");
    assert_eq!(admission["decision"], "resumed");
    assert_eq!(admission["attempt_id"], attempt.attempt_id);
    assert_eq!(
        admission["remaining"]["seconds"].as_u64(),
        Some(0),
        "the leftover allowance for this invocation is already spent"
    );
    write_report(
        &gated.fixture.runtime,
        &gated.task_id,
        &report(
            &attempt.attempt_id,
            ReviewVerdict::PassedWithoutRepairs,
            false,
        ),
    );
    let error = gated.settle(&admission).expect_err("over budget");
    let message = error.to_string();
    assert!(message.contains("review_minutes_exhausted"), "{message}");
    let certificate = gated.certificate();
    assert_eq!(certificate.verdict, ReviewVerdict::Incomplete);
    assert!(!certificate.verdict.passed());
    assert_eq!(certificate.budget.minutes, 1);
    assert!(certificate.consumed.seconds >= 120);
}

#[test]
fn task_drift_during_review_invalidates_the_gate_but_selector_additions_do_not() {
    let gated = gated_fixture(GATED_CONFIG);
    let admission = gated.admit().expect("admit");
    let attempt_id = admission["attempt_id"]
        .as_str()
        .expect("attempt id")
        .to_string();

    // A coupled selector the reviewer added through the task API is fine.
    gated
        .fixture
        .runtime
        .update_task(
            &gated.task_id,
            TaskUpdateParams {
                context_files: Some(vec!["file:src.txt".into(), "file:README.md".into()]),
                ..TaskUpdateParams::default()
            },
        )
        .expect("add selector");
    write_report(
        &gated.fixture.runtime,
        &gated.task_id,
        &report(&attempt_id, ReviewVerdict::PassedWithoutRepairs, false),
    );
    let settled = gated.settle(&admission).expect("selector growth passes");
    assert_eq!(settled["gate"], "passed");

    // Changed acceptance criteria re-establish review even with identical bytes.
    let drifted = gated_fixture(GATED_CONFIG);
    let admission = drifted.admit().expect("admit");
    let attempt_id = admission["attempt_id"]
        .as_str()
        .expect("attempt id")
        .to_string();
    drifted
        .fixture
        .runtime
        .update_task(
            &drifted.task_id,
            TaskUpdateParams {
                acceptance_criteria: Some(vec!["A different criterion.".into()]),
                ..TaskUpdateParams::default()
            },
        )
        .expect("edit criteria");
    write_report(
        &drifted.fixture.runtime,
        &drifted.task_id,
        &report(&attempt_id, ReviewVerdict::PassedWithoutRepairs, false),
    );
    let error = drifted.settle(&admission).expect_err("drift");
    assert!(
        error.to_string().contains("task_meaning_changed"),
        "{error}"
    );
}
