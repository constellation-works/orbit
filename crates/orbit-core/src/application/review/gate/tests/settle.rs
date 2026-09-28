//! Settling the gate: verdicts, certificates, budgets, and landing coverage.

use std::fs;

use orbit_types::task::TaskStatus;
use orbit_types::workflow::{
    REVIEW_GATE_ARTIFACT, REVIEW_MANIFEST_ARTIFACT, ReviewAttemptState, ReviewLedger, ReviewVerdict,
};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::automation::source::Source;
use crate::application::review::tests::{
    GATED_CONFIG, admit_input, admitted_run, fixture, git, implement_candidate, report, seed_task,
    settle_input, write_report,
};
use crate::application::review::{exclusions, review_gate_admit, review_gate_settle};
use crate::application::task::TaskUpdateParams;

use super::super::settle::{Checkpoint, interrupt_after};
use super::support::{Gated, gated_fixture, land_squash, page_for};

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
    assert_eq!(gated.settle(&admission).expect("unchanged replay"), settled);
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
fn replay_refuses_changed_task_meaning_with_the_same_candidate() {
    for change in ["criteria", "plan"] {
        let gated = gated_fixture(GATED_CONFIG);
        let admission = gated.admit().expect("admit");
        let attempt_id = admission["attempt_id"].as_str().expect("attempt id");
        write_report(
            &gated.fixture.runtime,
            &gated.task_id,
            &report(attempt_id, ReviewVerdict::PassedWithoutRepairs, false),
        );
        gated.settle(&admission).expect("initial pass");
        let certificate = gated.certificate();
        let store = gated.fixture.runtime.review_store().expect("store");
        let workspace_id = gated.fixture.runtime.workspace_id().expect("workspace");
        let ledger_before = store
            .review_ledger(&workspace_id, &certificate.lineage_key)
            .expect("ledger")
            .expect("present");
        let head = gated.head();

        let update = match change {
            "criteria" => TaskUpdateParams {
                acceptance_criteria: Some(vec!["Revised acceptance criterion.".into()]),
                ..TaskUpdateParams::default()
            },
            "plan" => TaskUpdateParams {
                plan: Some("Revised implementation plan.".into()),
                ..TaskUpdateParams::default()
            },
            _ => unreachable!(),
        };
        gated
            .fixture
            .runtime
            .update_task(&gated.task_id, update)
            .expect("change task meaning");
        let error = gated.settle(&admission).expect_err("stale review");
        assert!(
            error.to_string().contains("task_meaning_changed"),
            "{change}: {error}"
        );
        assert_eq!(gated.head(), head, "{change}: HEAD is unchanged");
        assert_eq!(
            store
                .review_ledger(&workspace_id, &certificate.lineage_key)
                .expect("ledger")
                .expect("present"),
            ledger_before,
            "{change}: replay must not consume review budget"
        );
    }
}

#[test]
fn replay_accepts_a_certificate_bound_to_widened_selectors_without_spending_budget() {
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

    let first = gated.settle(&admission).expect("initial pass");
    let certificate = gated.certificate();
    assert_eq!(certificate.selectors_widened, vec!["file:README.md"]);
    let head = gated.head();
    let store = gated.fixture.runtime.review_store().expect("store");
    let workspace_id = gated.fixture.runtime.workspace_id().expect("workspace");
    let ledger_before = store
        .review_ledger(&workspace_id, &certificate.lineage_key)
        .expect("ledger")
        .expect("present");

    let replay = gated
        .settle(&admission)
        .expect("replay widened certificate");
    assert_eq!(replay, first);
    assert_eq!(gated.head(), head, "replay creates no commit");
    assert_eq!(
        store
            .review_ledger(&workspace_id, &certificate.lineage_key)
            .expect("ledger")
            .expect("present"),
        ledger_before,
        "replay creates no attempt or budget charge"
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

/// One repair cycle only: a resumed settlement that counted its own charge
/// against the lineage would find none left and downgrade the pass.
const ONE_REPAIR_CONFIG: &str = "[crews.reviewers]\nmodel = \"review-model\"\nprovider = \"codex\"\nbackend = \"cli\"\n[crews.implementer]\nmodel = \"impl-model\"\nprovider = \"codex\"\nbackend = \"cli\"\n[workflow]\ndefault_crew = \"implementer\"\n[operation]\nreview_policy = \"before-pr\"\nreview_crew = \"reviewers\"\nreview_repair_cycles = 1\n";

/// Admit, let the reviewer edit the worktree, and report the repair.
fn admit_with_reviewer_repair(gated: &Gated) -> Value {
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
    admission
}

fn ledger(runtime: &OrbitRuntime, admission: &Value) -> ReviewLedger {
    runtime
        .review_store()
        .expect("store")
        .review_ledger(
            &runtime.workspace_id().expect("workspace"),
            admission["lineage_key"].as_str().expect("lineage key"),
        )
        .expect("ledger")
        .expect("present")
}

fn stored_certificate_exists(runtime: &OrbitRuntime, admission: &Value) -> bool {
    runtime
        .review_store()
        .expect("store")
        .review_certificate(
            &runtime.workspace_id().expect("workspace"),
            admission["attempt_id"].as_str().expect("attempt id"),
        )
        .expect("certificate lookup")
        .is_some()
}

fn verdict_comments(runtime: &OrbitRuntime, task_id: &str, attempt_id: &str) -> usize {
    let marker = format!("settled attempt `{attempt_id}`");
    runtime
        .get_task_comments(task_id)
        .expect("comments")
        .iter()
        .filter(|comment| comment.message.contains(&marker))
        .count()
}

#[test]
fn a_restart_after_the_repair_commit_settles_that_repair_once() {
    let gated = gated_fixture(ONE_REPAIR_CONFIG);
    let admission = admit_with_reviewer_repair(&gated);
    let runtime = &gated.fixture.runtime;

    interrupt_after(Checkpoint::RepairCommitted, 1);
    let error = gated.settle(&admission).expect_err("interrupted");
    assert!(error.to_string().contains("injected"), "{error}");
    let repair_head = gated.head();
    assert_ne!(
        repair_head, gated.implementation_sha,
        "the repair committed"
    );
    assert_eq!(
        ledger(runtime, &admission).attempts[0].state,
        ReviewAttemptState::Open,
        "the interruption preceded ledger settlement"
    );

    let settled = gated
        .settle(&admission)
        .expect("resume from the repair commit");
    assert_eq!(settled["verdict"], "passed_with_repairs");
    assert_eq!(settled["reviewed_head_sha"], repair_head);
    assert_eq!(settled["repair_commits"], json!([repair_head]));
    assert_eq!(gated.head(), repair_head, "no duplicate repair commit");
    assert_eq!(
        gated.author_of("HEAD"),
        "codex-reviewer <codex-reviewer@orbit.local>",
        "the repair keeps the reviewer's attribution"
    );

    let certificate = gated.certificate();
    assert_eq!(
        certificate.reviewed_candidate.commit,
        gated.implementation_sha
    );
    assert_eq!(certificate.final_candidate.commit, repair_head);
    assert_eq!(certificate.repair_commits.len(), 1);
    assert_eq!(
        certificate.repair_commits[0].author,
        "codex-reviewer <codex-reviewer@orbit.local>"
    );
    assert_eq!(certificate.consumed.reviewer_starts, 1);
    assert_eq!(certificate.consumed.repair_cycles, 1);
    let settled_ledger = ledger(runtime, &admission);
    assert_eq!(settled_ledger.attempts.len(), 1);
    assert_eq!(settled_ledger.consumed().repair_cycles, 1, "charged once");
    assert_eq!(
        verdict_comments(runtime, &gated.task_id, &certificate.attempt_id),
        1
    );
}

#[test]
fn a_commit_on_the_candidate_that_is_not_the_gates_repair_still_refuses() {
    let gated = gated_fixture(GATED_CONFIG);
    let admission = gated.admit().expect("admit");
    let attempt_id = admission["attempt_id"].as_str().expect("attempt id");
    write_report(
        &gated.fixture.runtime,
        &gated.task_id,
        &report(attempt_id, ReviewVerdict::PassedWithRepairs, true),
    );
    // The attempt trailer alone does not make a commit the gate's own.
    fs::write(gated.fixture.repo.join("src.txt"), "someone else\n").expect("edit");
    git(
        &gated.fixture.repo,
        &[
            "commit",
            "-am",
            &format!("edit\n\nOrbit-Review-Attempt: {attempt_id}"),
        ],
    );
    let head = gated.head();

    let error = gated.settle(&admission).expect_err("foreign commit");
    assert!(error.to_string().contains("candidate_changed"), "{error}");
    assert_eq!(gated.head(), head);
    assert_eq!(
        ledger(&gated.fixture.runtime, &admission).attempts[0].state,
        ReviewAttemptState::Open,
        "a refusal charges nothing"
    );
}

#[test]
fn a_restart_after_ledger_settlement_finishes_the_recorded_outcome() {
    let gated = gated_fixture(ONE_REPAIR_CONFIG);
    let admission = admit_with_reviewer_repair(&gated);
    let runtime = &gated.fixture.runtime;

    interrupt_after(Checkpoint::LedgerSettled, 1);
    gated.settle(&admission).expect_err("interrupted");
    let charged = ledger(runtime, &admission);
    assert_eq!(
        charged.attempts[0].state,
        ReviewAttemptState::Settled {
            verdict: ReviewVerdict::PassedWithRepairs
        }
    );
    assert!(!stored_certificate_exists(runtime, &admission));
    let repair_head = gated.head();

    let settled = gated.settle(&admission).expect("finish the settlement");
    assert_eq!(settled["verdict"], "passed_with_repairs");
    assert_eq!(settled["reviewed_head_sha"], repair_head);
    assert_eq!(gated.head(), repair_head, "no second repair commit");
    assert_eq!(
        ledger(runtime, &admission),
        charged,
        "finishing never charges the ledger again"
    );
    let certificate = gated.certificate();
    assert_eq!(certificate.verdict, ReviewVerdict::PassedWithRepairs);
    assert_eq!(certificate.consumed, charged.consumed());
    assert_eq!(certificate.repair_commits[0].commit, repair_head);
    assert!(stored_certificate_exists(runtime, &admission));
    assert_eq!(gated.settle(&admission).expect("replay"), settled);
}

#[test]
fn a_settled_attempt_whose_task_meaning_drifted_refuses_to_finish() {
    let gated = gated_fixture(GATED_CONFIG);
    let admission = gated.admit().expect("admit");
    let attempt_id = admission["attempt_id"].as_str().expect("attempt id");
    write_report(
        &gated.fixture.runtime,
        &gated.task_id,
        &report(attempt_id, ReviewVerdict::PassedWithoutRepairs, false),
    );
    interrupt_after(Checkpoint::LedgerSettled, 1);
    gated.settle(&admission).expect_err("interrupted");
    let charged = ledger(&gated.fixture.runtime, &admission);

    gated
        .fixture
        .runtime
        .update_task(
            &gated.task_id,
            TaskUpdateParams {
                acceptance_criteria: Some(vec!["Revised acceptance criterion.".into()]),
                ..TaskUpdateParams::default()
            },
        )
        .expect("change task meaning");
    let error = gated.settle(&admission).expect_err("drifted");
    let message = error.to_string();
    assert!(message.contains("settlement_diverged"), "{message}");
    assert!(message.contains("task_meaning_changed"), "{message}");
    assert_eq!(ledger(&gated.fixture.runtime, &admission), charged);
    assert!(!stored_certificate_exists(
        &gated.fixture.runtime,
        &admission
    ));
}

#[test]
fn a_restart_after_the_certificate_restores_every_bundle_task_artifact_once() {
    let fixture = fixture(GATED_CONFIG);
    let runtime = &fixture.runtime;
    let task_ids = vec![
        seed_task(runtime, "bundle first").id,
        seed_task(runtime, "bundle second").id,
    ];
    let run = admitted_run(runtime, "task_pr_pipeline", &task_ids);
    implement_candidate(&fixture.repo, &task_ids[0]);
    let admission = review_gate_admit(
        runtime,
        "review_gate_admit",
        &admit_input(&run.run_id, &task_ids, &fixture.repo),
    )
    .expect("admit");
    let attempt_id = admission["attempt_id"].as_str().expect("attempt id");
    write_report(
        runtime,
        &task_ids[0],
        &report(attempt_id, ReviewVerdict::PassedWithoutRepairs, false),
    );
    let settle = || {
        review_gate_settle(
            runtime,
            "review_gate_settle",
            &settle_input(&run.run_id, &task_ids, &fixture.repo, &admission),
        )
    };

    interrupt_after(Checkpoint::TaskPublished, 1);
    settle().expect_err("interrupted");
    assert!(stored_certificate_exists(runtime, &admission));
    assert!(
        runtime
            .get_task_artifact(&task_ids[1], REVIEW_GATE_ARTIFACT)
            .expect("read")
            .is_none(),
        "the second task was not published before the interruption"
    );

    let settled = settle().expect("restore and pass");
    assert_eq!(settled["gate"], "passed");
    let replay = settle().expect("idempotent replay");
    assert_eq!(replay, settled);
    let published = task_ids
        .iter()
        .map(|task_id| {
            runtime
                .get_task_artifact(task_id, REVIEW_GATE_ARTIFACT)
                .expect("read")
                .expect("every bundle task carries the certificate")
                .content
        })
        .collect::<Vec<_>>();
    assert_eq!(published[0], published[1]);
    for task_id in &task_ids {
        assert_eq!(
            verdict_comments(runtime, task_id, attempt_id),
            1,
            "{task_id}: the verdict is disclosed exactly once"
        );
    }
}

#[test]
fn a_restart_after_the_certificate_refuses_a_changed_candidate_without_publishing() {
    let gated = gated_fixture(GATED_CONFIG);
    let admission = gated.admit().expect("admit");
    let attempt_id = admission["attempt_id"].as_str().expect("attempt id");
    write_report(
        &gated.fixture.runtime,
        &gated.task_id,
        &report(attempt_id, ReviewVerdict::PassedWithoutRepairs, false),
    );
    interrupt_after(Checkpoint::CertificateRecorded, 1);
    gated.settle(&admission).expect_err("interrupted");
    assert!(stored_certificate_exists(
        &gated.fixture.runtime,
        &admission
    ));

    fs::write(gated.fixture.repo.join("src.txt"), "unrelated edit\n").expect("edit");
    git(&gated.fixture.repo, &["commit", "-am", "unrelated"]);
    let error = gated.settle(&admission).expect_err("drifted candidate");
    assert!(error.to_string().contains("candidate_changed"), "{error}");
    assert!(
        gated
            .fixture
            .runtime
            .get_task_artifact(&gated.task_id, REVIEW_GATE_ARTIFACT)
            .expect("read")
            .is_none(),
        "a refused replay publishes nothing"
    );

    git(&gated.fixture.repo, &["reset", "--hard", "HEAD~1"]);
    let settled = gated
        .settle(&admission)
        .expect("restore on the certified head");
    assert_eq!(settled["gate"], "passed");
    assert_eq!(gated.certificate().attempt_id, attempt_id);
}

#[test]
fn a_resumed_repair_keeps_its_declared_widening_and_still_downgrades_drive_bys() {
    for undeclared in [false, true] {
        let gated = gated_fixture(GATED_CONFIG);
        let admission = gated.admit().expect("admit");
        let attempt_id = admission["attempt_id"].as_str().expect("attempt id");
        fs::write(
            gated.fixture.repo.join("README.md"),
            "coupled repair of derived artifact\n",
        )
        .expect("repair");
        if undeclared {
            fs::write(gated.fixture.repo.join("stray.txt"), "drive-by\n").expect("stray");
        }
        let mut claim = report(attempt_id, ReviewVerdict::PassedWithRepairs, true);
        claim.findings[0].paths = vec!["README.md".to_string()];
        write_report(&gated.fixture.runtime, &gated.task_id, &claim);

        interrupt_after(Checkpoint::RepairCommitted, 1);
        gated.settle(&admission).expect_err("interrupted");
        let resumed = gated.settle(&admission);
        let certificate = gated.certificate();
        assert_eq!(
            certificate.selectors_widened,
            vec!["file:README.md"],
            "undeclared={undeclared}"
        );
        if undeclared {
            let error = resumed.expect_err("a drive-by is never laundered by a restart");
            assert!(error.to_string().contains("repair_out_of_scope"), "{error}");
            assert_eq!(certificate.verdict, ReviewVerdict::Incomplete);
        } else {
            assert_eq!(resumed.expect("pass")["verdict"], "passed_with_repairs");
        }
    }
}
