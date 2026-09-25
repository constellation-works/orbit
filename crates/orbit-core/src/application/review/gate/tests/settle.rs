//! Settling the gate: verdicts, certificates, budgets, and landing coverage.

use std::fs;

use orbit_types::task::TaskStatus;
use orbit_types::workflow::{REVIEW_MANIFEST_ARTIFACT, ReviewVerdict};
use serde_json::{Value, json};

use crate::application::automation::source::Source;
use crate::application::review::exclusions;
use crate::application::review::tests::{GATED_CONFIG, git, report, write_report};
use crate::application::task::TaskUpdateParams;

use super::support::{gated_fixture, land_squash, page_for};

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
