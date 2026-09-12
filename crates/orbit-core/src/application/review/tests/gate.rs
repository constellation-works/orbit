//! The before-PR review gate over a real checkout [ORB-11333].

use std::collections::BTreeMap;
use std::fs;

use chrono::{Duration, Utc};
use orbit_automation::review::{combined_task_meaning_digest, task_meaning_digest};
use orbit_store::contracts::ReviewReserveRequest;
use orbit_types::task::TaskStatus;
use orbit_types::workflow::automation::{AutomationState, Delivery, SourcePage, SourceRevision};
use orbit_types::workflow::{
    REVIEW_CONTRACT_VERSION, REVIEW_GATE_ARTIFACT, REVIEW_MANIFEST_ARTIFACT, ReviewAttemptState,
    ReviewBudget, ReviewCertificate, ReviewReservation, ReviewVerdict, ValidationOutcome,
    ValidationRole,
};
use serde_json::{Value, json};

use super::{
    Fixture, GATED_CONFIG, admit_input, admitted_run, fixture, git, implement_candidate, report,
    seed_task, settle_input, validation, write_report,
};
use crate::application::automation::source::Source;
use crate::application::review::{exclusions, lineage_key, review_gate_admit, review_gate_settle};
use crate::application::task::TaskUpdateParams;

/// Fixture with a task, its admitted run, and a checked-out candidate.
struct Gated {
    fixture: Fixture,
    task_id: String,
    run_id: String,
    implementation_sha: String,
}

fn gated_fixture(config: &str) -> Gated {
    let fixture = fixture(config);
    let task = seed_task(&fixture.runtime, "gated change");
    let run = admitted_run(
        &fixture.runtime,
        "task_pr_pipeline",
        std::slice::from_ref(&task.id),
    );
    let implementation_sha = implement_candidate(&fixture.repo, &task.id);
    Gated {
        fixture,
        task_id: task.id,
        run_id: run.run_id,
        implementation_sha,
    }
}

impl Gated {
    fn admit(&self) -> Result<Value, orbit_engine::DispatchError> {
        review_gate_admit(
            &self.fixture.runtime,
            "review_gate_admit",
            &admit_input(
                &self.run_id,
                std::slice::from_ref(&self.task_id),
                &self.fixture.repo,
            ),
        )
    }

    fn settle(&self, admission: &Value) -> Result<Value, orbit_engine::DispatchError> {
        review_gate_settle(
            &self.fixture.runtime,
            "review_gate_settle",
            &settle_input(
                &self.run_id,
                std::slice::from_ref(&self.task_id),
                &self.fixture.repo,
                admission,
            ),
        )
    }

    fn certificate(&self) -> ReviewCertificate {
        let artifact = self
            .fixture
            .runtime
            .get_task_artifact(&self.task_id, REVIEW_GATE_ARTIFACT)
            .expect("read")
            .expect("certificate artifact");
        serde_json::from_slice(&artifact.content).expect("certificate json")
    }

    fn head(&self) -> String {
        git(&self.fixture.repo, &["rev-parse", "HEAD"])
    }

    fn author_of(&self, spec: &str) -> String {
        git(
            &self.fixture.repo,
            &["log", "-1", "--format=%an <%ae>", spec],
        )
    }

    fn rescope(&self, selectors: &[&str]) {
        self.fixture
            .runtime
            .update_task(
                &self.task_id,
                TaskUpdateParams {
                    context_files: Some(
                        selectors
                            .iter()
                            .map(|selector| (*selector).into())
                            .collect(),
                    ),
                    ..TaskUpdateParams::default()
                },
            )
            .expect("rescope");
    }

    fn context_files(&self) -> Vec<String> {
        self.fixture
            .runtime
            .get_task(&self.task_id)
            .expect("task")
            .context_files
    }
}

#[test]
fn a_clean_review_passes_without_repairs_and_pins_the_reviewed_candidate() {
    let gated = gated_fixture(GATED_CONFIG);
    let admission = gated.admit().expect("admit");
    assert_eq!(admission["applies"], true);
    assert_eq!(admission["decision"], "admitted");
    assert_eq!(admission["reviewer"]["crew"], "reviewers");
    assert_eq!(admission["reviewer"]["model"], "review-model");
    assert_eq!(admission["head_sha"], gated.implementation_sha);
    assert_eq!(admission["implementation_commit_count"], 1);
    assert!(
        gated
            .fixture
            .runtime
            .get_task_artifact(&gated.task_id, REVIEW_MANIFEST_ARTIFACT)
            .expect("read")
            .is_some(),
        "the reviewer manifest is a task artifact"
    );

    let attempt_id = admission["attempt_id"].as_str().expect("attempt id");
    write_report(
        &gated.fixture.runtime,
        &gated.task_id,
        &report(attempt_id, ReviewVerdict::PassedWithoutRepairs, false),
    );
    let settled = gated.settle(&admission).expect("settle");
    assert_eq!(settled["gate"], "passed");
    assert_eq!(settled["verdict"], "passed_without_repairs");
    assert_eq!(settled["assurance"], "independent_review");
    assert_eq!(settled["reviewed_head_sha"], gated.implementation_sha);
    assert_eq!(settled["repair_commits"], json!([]));

    let certificate = gated.certificate();
    assert_eq!(certificate.verdict, ReviewVerdict::PassedWithoutRepairs);
    assert!(certificate.validation_complete);
    assert_eq!(certificate.consumed.reviewer_starts, 1);
    assert_eq!(certificate.consumed.repair_cycles, 0);
    assert!(!certificate.reviewer.same_model_as_implementer);
    assert_eq!(
        certificate.implementation_commits[0].author,
        "orbit[impl-model] <agent@orbit.invalid>"
    );
    let comments = gated
        .fixture
        .runtime
        .get_task_comments(&gated.task_id)
        .expect("comments");
    assert!(
        comments.last().is_some_and(|comment| comment
            .message
            .contains("verdict **passed_without_repairs**")),
        "the verdict is disclosed on the task"
    );
    assert_eq!(
        gated
            .fixture
            .runtime
            .get_task(&gated.task_id)
            .expect("task")
            .status,
        TaskStatus::InProgress,
        "a pass is evidence, not a lifecycle transition"
    );
}

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
fn changes_required_blocks_publication_and_keeps_the_certificate_out_of_coverage() {
    let gated = gated_fixture(GATED_CONFIG);
    let admission = gated.admit().expect("admit");
    let attempt_id = admission["attempt_id"].as_str().expect("attempt id");
    write_report(
        &gated.fixture.runtime,
        &gated.task_id,
        &report(attempt_id, ReviewVerdict::ChangesRequired, false),
    );
    let error = gated.settle(&admission).expect_err("blocked");
    let message = error.to_string();
    assert!(message.contains("review_gate_blocked"), "{message}");
    assert!(message.contains("changes_required"), "{message}");

    let certificate = gated.certificate();
    assert_eq!(certificate.verdict, ReviewVerdict::ChangesRequired);
    assert_eq!(
        certificate.escalation.as_deref(),
        Some("decide whether the note is required")
    );
    assert!(certificate.assurance.is_none());
    let store = gated.fixture.runtime.review_store().expect("store");
    assert!(
        store
            .review_certificates_for_tree(
                &certificate.repository,
                &certificate.final_candidate.tree,
                5
            )
            .expect("lookup")
            .is_empty(),
        "a non-pass never indexes as coverage"
    );
}

#[test]
fn a_missing_or_stale_report_is_incomplete_never_a_pass() {
    let gated = gated_fixture(GATED_CONFIG);
    let admission = gated.admit().expect("admit");
    let error = gated.settle(&admission).expect_err("no report");
    assert!(error.to_string().contains("incomplete"), "{error}");
    let certificate = gated.certificate();
    assert_eq!(certificate.verdict, ReviewVerdict::Incomplete);
    assert!(
        certificate
            .escalation
            .as_deref()
            .is_some_and(|reason| reason.contains("report_missing"))
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
fn budgets_persist_across_attempts_and_interrupted_attempts_resume() {
    let gated = gated_fixture(
        "[crews.reviewers]\nmodel = \"review-model\"\nprovider = \"codex\"\nbackend = \"cli\"\n[crews.implementer]\nmodel = \"impl-model\"\nprovider = \"codex\"\nbackend = \"cli\"\n[workflow]\ndefault_crew = \"implementer\"\n[operation]\nreview_policy = \"before-pr\"\nreview_crew = \"reviewers\"\nreview_reviewer_starts = 1\n",
    );
    let first = gated.admit().expect("admit");
    assert_eq!(first["decision"], "admitted");

    // A restart before settlement resumes the same attempt at no cost.
    let resumed = gated.admit().expect("resume");
    assert_eq!(resumed["decision"], "resumed");
    assert_eq!(resumed["attempt_id"], first["attempt_id"]);

    let attempt_id = first["attempt_id"].as_str().expect("attempt id");
    write_report(
        &gated.fixture.runtime,
        &gated.task_id,
        &report(attempt_id, ReviewVerdict::ChangesRequired, false),
    );
    gated.settle(&first).expect_err("blocked");

    // A new candidate on the same lineage finds the single start spent.
    fs::write(gated.fixture.repo.join("src.txt"), "second candidate\n").expect("edit");
    git(&gated.fixture.repo, &["commit", "-am", "second attempt"]);
    let error = gated.admit().expect_err("exhausted");
    assert!(
        error.to_string().contains("review_budget_exhausted"),
        "{error}"
    );
    assert!(
        error.to_string().contains("review_starts_exhausted"),
        "{error}"
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

#[test]
fn reviewer_crew_must_be_configured_resolvable_and_allowed() {
    let unconfigured = gated_fixture(
        "[crews.implementer]\nmodel = \"impl-model\"\nprovider = \"codex\"\nbackend = \"cli\"\n[workflow]\ndefault_crew = \"implementer\"\n[operation]\nreview_policy = \"before-pr\"\n",
    );
    let error = unconfigured.admit().expect_err("no crew");
    assert!(
        error.to_string().contains("review_crew_unconfigured"),
        "{error}"
    );

    let missing = gated_fixture(
        "[crews.implementer]\nmodel = \"impl-model\"\nprovider = \"codex\"\nbackend = \"cli\"\n[workflow]\ndefault_crew = \"implementer\"\n[operation]\nreview_policy = \"before-pr\"\nreview_crew = \"ghosts\"\n",
    );
    let error = missing.admit().expect_err("unknown crew");
    assert!(
        error.to_string().contains("review_crew_unavailable"),
        "{error}"
    );

    // The run's crew allowlist excludes the reviewer: escalate, never substitute.
    let excluded = gated_fixture(GATED_CONFIG);
    let mut input = admit_input(
        &excluded.run_id,
        std::slice::from_ref(&excluded.task_id),
        &excluded.fixture.repo,
    );
    let run = excluded
        .fixture
        .runtime
        .get_job_run_backend(&excluded.run_id)
        .expect("run")
        .expect("present");
    let mut run_input = run.input.clone().expect("input");
    run_input["allowed_crews"] = json!(["implementer"]);
    // Persist the narrower allowlist as the drain would have captured it.
    let narrowed = orbit_engine::RuntimeHost::insert_job_run(
        &excluded.fixture.runtime,
        "task_pr_pipeline",
        1,
        Utc::now(),
        Some(run_input),
        None,
    )
    .expect("insert narrowed run");
    excluded
        .fixture
        .runtime
        .update_task(
            &excluded.task_id,
            TaskUpdateParams {
                job_run_id: Some(Some(narrowed.run_id.clone())),
                ..TaskUpdateParams::default()
            },
        )
        .expect("rebind");
    input["job_run_id"] = json!(narrowed.run_id);
    let error = review_gate_admit(&excluded.fixture.runtime, "review_gate_admit", &input)
        .expect_err("excluded crew");
    assert!(
        error.to_string().contains("review_crew_excluded"),
        "{error}"
    );
}

#[test]
fn the_gate_reads_the_captured_snapshot_not_the_live_preference() {
    // Captured `before-pr`, then the workspace rolls back to `none`: the
    // active gate still applies.
    let gated = gated_fixture(GATED_CONFIG);
    fs::write(
        gated.fixture.repo.join(".orbit/config.toml"),
        "[crews.reviewers]\nmodel = \"review-model\"\nprovider = \"codex\"\nbackend = \"cli\"\n[operation]\nreview_policy = \"none\"\n",
    )
    .expect("roll back");
    let admission = gated.admit().expect("admit");
    assert_eq!(admission["applies"], true);
    assert_eq!(admission["timing_source"], "workspace");

    // Captured `none`, later switched to `before-pr`: no retroactive gate.
    let ungated = gated_fixture("[operation]\nreview_policy = \"none\"\n");
    let admission = ungated.admit().expect("admit");
    assert_eq!(admission["applies"], false);
    assert_eq!(admission["reason"], "review_policy_none");
    let settled = ungated.settle(&admission).expect("not required");
    assert_eq!(settled["gate"], "not_required");
    assert_eq!(settled["reviewed_head_sha"], "");

    // A checked no-diff exemption never creates coverage.
    let no_diff = gated_fixture(GATED_CONFIG);
    let mut input = admit_input(
        &no_diff.run_id,
        std::slice::from_ref(&no_diff.task_id),
        &no_diff.fixture.repo,
    );
    input["skipped_no_diff_expected"] = json!(true);
    let admission =
        review_gate_admit(&no_diff.fixture.runtime, "review_gate_admit", &input).expect("exempt");
    assert_eq!(admission["applies"], false);
    assert_eq!(admission["reason"], "no_diff_exemption");

    // A local route under a before-pr admission refuses rather than skipping.
    let mut local = admit_input(
        &no_diff.run_id,
        std::slice::from_ref(&no_diff.task_id),
        &no_diff.fixture.repo,
    );
    local["mode"] = json!("local");
    let error = review_gate_admit(&no_diff.fixture.runtime, "review_gate_admit", &local)
        .expect_err("local route");
    assert!(
        error
            .to_string()
            .contains("review_policy_local_route_refused"),
        "{error}"
    );
}

#[test]
fn epic_worktree_token_does_not_mask_the_admitted_run() {
    let gated = gated_fixture(GATED_CONFIG);
    let worktree_token = format!("epic-{}", gated.task_id);
    let mut input = admit_input(
        &worktree_token,
        std::slice::from_ref(&gated.task_id),
        &gated.fixture.repo,
    );

    let missing = review_gate_admit(&gated.fixture.runtime, "review_gate_admit", &input)
        .expect("a worktree token is not a run record");
    assert_eq!(missing["applies"], false);
    assert_eq!(missing["reason"], "review_admission_missing");

    input["run_id"] = json!(&gated.run_id);
    let admission = review_gate_admit(&gated.fixture.runtime, "review_gate_admit", &input)
        .expect("the injected admitted run still applies");
    assert_eq!(admission["applies"], true);
    assert_eq!(admission["decision"], "admitted");
    assert_eq!(admission["reviewer"]["crew"], "reviewers");
}

/// Land the checked-out candidate onto `main` the way a squash merge does
/// and return the delivery the source adapter would observe.
fn land_squash(gated: &Gated, repository: &str) -> Delivery {
    let repo = &gated.fixture.repo;
    let candidate = git(repo, &["rev-parse", "--abbrev-ref", "HEAD"]);
    git(repo, &["checkout", "main"]);
    let before = git(repo, &["rev-parse", "HEAD"]);
    let before_tree = git(repo, &["rev-parse", "HEAD^{tree}"]);
    git(repo, &["merge", "--squash", &candidate]);
    git(repo, &["commit", "-m", &format!("squash {candidate}")]);
    let after = git(repo, &["rev-parse", "HEAD"]);
    let after_tree = git(repo, &["rev-parse", "HEAD^{tree}"]);
    Delivery {
        key: format!("pr:{repository}:main:{}", &after[..8]),
        repository: repository.to_string(),
        branch: "main".to_string(),
        before: orbit_types::workflow::automation::SourceRevision {
            commit: before,
            tree: before_tree,
        },
        after: orbit_types::workflow::automation::SourceRevision {
            commit: after.clone(),
            tree: after_tree,
        },
        commits: vec![after],
        task_ids: vec![],
        evidence_reference: "https://github.com/example/pull/1".to_string(),
        evidence_digest: "digest".to_string(),
        landed_at: Utc::now(),
    }
}

fn page_for(delivery: &Delivery) -> (AutomationState, SourcePage) {
    let state = AutomationState {
        members: None,
        consumer: "hm/ws/auto-task/delivery-code-review".into(),
        epoch: "e".into(),
        trigger: None,
        repository: delivery.repository.clone(),
        branch: "main".into(),
        generation: 1,
        baseline: delivery.before.clone(),
        observed: delivery.before.clone(),
        covered: delivery.before.clone(),
        pending_commits: vec![],
        pending: vec![],
        waived: vec![],
        excluded: vec![],
        unresolved: BTreeMap::new(),
        associations: BTreeMap::new(),
        active: None,
    };
    let page = SourcePage {
        from: delivery.before.clone(),
        through: delivery.after.clone(),
        commits: delivery.commits.clone(),
        deliveries: vec![delivery.clone()],
        unresolved: BTreeMap::new(),
        associations: BTreeMap::new(),
        exclusions: BTreeMap::new(),
        complete: true,
    };
    (state, page)
}

#[test]
fn a_passed_certificate_excludes_the_exact_landing_and_nothing_else() {
    let gated = gated_fixture(GATED_CONFIG);
    let admission = gated.admit().expect("admit");
    let attempt_id = admission["attempt_id"]
        .as_str()
        .expect("attempt id")
        .to_string();
    write_report(
        &gated.fixture.runtime,
        &gated.task_id,
        &report(&attempt_id, ReviewVerdict::PassedWithoutRepairs, false),
    );
    gated.settle(&admission).expect("pass");
    let runtime = &gated.fixture.runtime;
    let source = Source::new(&gated.fixture.repo);
    let repository = source.repository().expect("repository");

    // Squash onto the reviewed base reproduces the reviewed tree: excluded.
    let delivery = land_squash(&gated, &repository);
    let (state, mut page) = page_for(&delivery);
    exclusions(runtime, &source, &state, &mut page).expect("exclusions");
    let exclusion = page
        .exclusions
        .get(&delivery.key)
        .expect("covered landing is excluded");
    assert_eq!(exclusion.attempt_id, attempt_id);
    assert_eq!(exclusion.assurance, "independent_review");

    // Editing the task's criteria after landing withdraws the exclusion.
    runtime
        .update_task(
            &gated.task_id,
            TaskUpdateParams {
                acceptance_criteria: Some(vec!["Rewritten criterion.".into()]),
                ..TaskUpdateParams::default()
            },
        )
        .expect("edit");
    let (state, mut page) = page_for(&delivery);
    exclusions(runtime, &source, &state, &mut page).expect("exclusions");
    assert!(page.exclusions.is_empty(), "task drift is uncovered");
}

#[test]
fn a_landing_on_a_moved_base_or_with_later_edits_stays_uncovered() {
    let gated = gated_fixture(GATED_CONFIG);
    let admission = gated.admit().expect("admit");
    let attempt_id = admission["attempt_id"]
        .as_str()
        .expect("attempt id")
        .to_string();
    write_report(
        &gated.fixture.runtime,
        &gated.task_id,
        &report(&attempt_id, ReviewVerdict::PassedWithoutRepairs, false),
    );
    gated.settle(&admission).expect("pass");
    let runtime = &gated.fixture.runtime;
    let repo = &gated.fixture.repo;
    let source = Source::new(repo);
    let repository = source.repository().expect("repository");

    // Something else lands on main first: the base tree moved.
    let candidate = git(repo, &["rev-parse", "--abbrev-ref", "HEAD"]);
    git(repo, &["checkout", "main"]);
    fs::write(repo.join("README.md"), "moved base\n").expect("edit main");
    git(repo, &["commit", "-am", "unrelated landing"]);
    git(repo, &["checkout", &candidate]);
    let moved = land_squash(&gated, &repository);
    let (state, mut page) = page_for(&moved);
    exclusions(runtime, &source, &state, &mut page).expect("exclusions");
    assert!(
        page.exclusions.is_empty(),
        "a different base tree is uncovered"
    );

    // A later edit on the candidate before landing changes the final tree.
    let edited = gated_fixture(GATED_CONFIG);
    let admission = edited.admit().expect("admit");
    let attempt_id = admission["attempt_id"]
        .as_str()
        .expect("attempt id")
        .to_string();
    write_report(
        &edited.fixture.runtime,
        &edited.task_id,
        &report(&attempt_id, ReviewVerdict::PassedWithoutRepairs, false),
    );
    edited.settle(&admission).expect("pass");
    fs::write(edited.fixture.repo.join("src.txt"), "edited after review\n").expect("edit");
    git(&edited.fixture.repo, &["commit", "-am", "post-review edit"]);
    let source = Source::new(&edited.fixture.repo);
    let repository = source.repository().expect("repository");
    let landed = land_squash(&edited, &repository);
    let (state, mut page) = page_for(&landed);
    exclusions(&edited.fixture.runtime, &source, &state, &mut page).expect("exclusions");
    assert!(page.exclusions.is_empty(), "later edits are uncovered");
}

#[test]
fn externally_completed_merge_is_audited_and_keeps_review_coverage_open() {
    let gated = gated_fixture(GATED_CONFIG);
    let admission = gated.admit().expect("admit");
    let attempt_id = admission["attempt_id"].as_str().expect("attempt id");
    write_report(
        &gated.fixture.runtime,
        &gated.task_id,
        &report(attempt_id, ReviewVerdict::PassedWithoutRepairs, false),
    );
    gated.settle(&admission).expect("pass");
    let runtime = &gated.fixture.runtime;
    let source = Source::new(&gated.fixture.repo);
    let repository = source.repository().expect("repository");
    let delivery = land_squash(&gated, &repository);

    crate::application::review::record_review_landing(
        runtime,
        &orbit_engine::ReviewLandingRequest {
            run_id: gated.run_id.clone(),
            task_ids: vec![gated.task_id.clone()],
            workspace_path: gated.fixture.repo.clone(),
            pr_number: "42".into(),
            base: "main".into(),
            reviewed_head_sha: gated.implementation_sha.clone(),
            managed_merge: false,
            landed_commit: Some(delivery.after.commit.clone()),
        },
    )
    .expect("record observed external landing");
    let landings = runtime
        .review_store()
        .expect("review store")
        .review_landings(attempt_id)
        .expect("landings");
    assert_eq!(landings.len(), 1);
    assert!(!landings[0].covered);
    assert_eq!(landings[0].reason.as_deref(), Some("external_landing_race"));
    assert_eq!(landings[0].landed.commit, delivery.after.commit);

    let (state, mut page) = page_for(&delivery);
    exclusions(runtime, &source, &state, &mut page).expect("coverage");
    assert!(
        page.exclusions.is_empty(),
        "even the same tree stays uncovered after an external merge"
    );
    let audits = runtime
        .list_audit_events(None, None, None, None, 100)
        .expect("audits");
    assert!(
        audits
            .iter()
            .filter_map(|audit| audit.arguments_json.as_deref())
            .filter_map(|args| serde_json::from_str::<Value>(args).ok())
            .any(|args| args["phase"] == "landing"
                && args["managed_merge"] == false
                && args["reason"] == "external_landing_race"),
        "external landing audit carries its provenance"
    );
}
