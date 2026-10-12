//! A run that captured `review.before_landing` reviews its published PR, not
//! its unpublished candidate, and any outcome but an approve keeps that PR
//! open and unmerged with the task in review [ORB-14849].

use std::path::{Path, PathBuf};
use std::process::Command;

use chrono::Utc;
use orbit_core::TaskStatus;
use orbit_engine::{RuntimeHost, TaskAutomationUpdate, execute_deterministic_action};
use orbit_types::task::{ExternalRef, GITHUB_PR_EXTERNAL_REF_SYSTEM};
use orbit_types::workflow::{JobRunState, REVIEW_CONTRACT_VERSION};
use serde_json::{Value, json};

use super::review_gate_audit::Fixture;

#[test]
fn an_owner_before_landing_review_keeps_the_durable_task_summary() {
    if !super::dispatch_admission::isolated(
        "review_before_landing::an_owner_before_landing_review_keeps_the_durable_task_summary",
    ) {
        return;
    }
    let mut fixture = Fixture::before_landing();
    fixture
        .runtime
        .update_task_with_identity(
            &fixture.task_id,
            orbit_core::application::task::TaskUpdateParams {
                execution_summary: Some(
                    "Outcome: success\nDurable owner implementation account.".into(),
                ),
                ..Default::default()
            },
            Some("codex".into()),
            None,
        )
        .unwrap();
    let durable = fixture
        .runtime
        .get_task(&fixture.task_id)
        .unwrap()
        .execution_summary;
    let run_id = fixture.input["job_run_id"].as_str().unwrap();
    fixture
        .runtime
        .checkpoint_step(
            run_id,
            0,
            "implement_bundle",
            &json!({}),
            &std::collections::BTreeMap::from([(
                "implement_one".into(),
                json!({"execution_summary": "a different implementer account"}),
            )]),
        )
        .unwrap();
    fixture.input["before_landing"] = json!(true);
    fixture.admit();
    let manifest: orbit_types::workflow::ReviewManifest = serde_json::from_slice(
        &fixture
            .runtime
            .get_task_artifact(
                &fixture.task_id,
                orbit_types::workflow::REVIEW_MANIFEST_ARTIFACT,
            )
            .unwrap()
            .unwrap()
            .content,
    )
    .unwrap();
    assert_eq!(manifest.implementer_summaries[&fixture.task_id], durable);
}

/// The pre-push gate does not apply to a before-landing run, the gate after
/// `pr_open` does, and a rejecting reviewer refuses its settlement. The
/// failure handoff then keeps the published PR and the task in review under
/// the failed step's typed reason — for a reject, an incomplete and a timed
/// out reviewer alike — and finalizing the failed run does not block it.
#[test]
fn a_before_landing_review_that_does_not_approve_keeps_the_pr_open_in_review() {
    if !super::dispatch_admission::isolated(
        "review_before_landing::a_before_landing_review_that_does_not_approve_keeps_the_pr_open_in_review",
    ) {
        return;
    }
    let mut fixture = Fixture::before_landing();
    let pre_push = fixture
        .runtime
        .run_deterministic(
            "review_gate_admit",
            &json!({}),
            &fixture.input,
            Default::default(),
        )
        .unwrap();
    assert_eq!(pre_push["applies"], false, "{pre_push}");
    assert_eq!(pre_push["reason"], "review_before_landing");
    assert_eq!(pre_push["timing"], "before-landing");

    // `pr_open` and `pr_promote` ran: the task is in review with its PR.
    published(&fixture);
    fixture.input["before_landing"] = json!(true);
    fixture.admit();
    let admission = &fixture.input["admission"];
    assert_eq!(admission["applies"], true, "{admission}");
    assert_eq!(admission["timing"], "before-landing");

    fixture.put_report(&json!({
        "schema_version": REVIEW_CONTRACT_VERSION,
        "attempt_id": fixture.input["admission"]["attempt_id"],
        "verdict": "reject", "summary": "Checked the published PR.",
        "findings": [],
        "validation": [{"id": "V1", "command": "fixture check", "outcome": "passed", "role": "required"}],
        "escalation": "The change is wrong.",
    }));
    let refused = fixture
        .settle()
        .expect_err("a rejecting review never approves the merge")
        .to_string();
    assert!(
        refused.contains("review_gate_blocked") && refused.contains("stays open and unmerged"),
        "{refused}"
    );
    assert_eq!(status(&fixture), TaskStatus::Review);

    for (failed_step, message, reason) in [
        (
            "landing_review_gate_settle",
            refused.as_str(),
            "review_gate_blocked",
        ),
        (
            "landing_review_gate_settle",
            "review_gate_blocked: verdict incomplete (missing evidence); 0 finding(s) recorded",
            "review_gate_blocked",
        ),
        (
            "landing_review",
            "review_timeout_incomplete: reviewer exceeded its wall clock",
            "review_timeout_incomplete",
        ),
    ] {
        let handoff = failure_handoff(&fixture, failed_step, message);
        assert_eq!(handoff["decision"], "landing_review_failure", "{handoff}");
        assert_eq!(handoff["reason"], reason, "{handoff}");
        assert_eq!(handoff["pr_number"], "42");
        assert_eq!(handoff["task_status"], "review");
        assert_eq!(status(&fixture), TaskStatus::Review);
    }
    let comments = fixture.runtime.get_task_comments(&fixture.task_id).unwrap();
    assert!(
        comments.iter().any(|comment| comment.message.starts_with(
            "Before-landing review did not approve PR #42 (`review_timeout_incomplete`)"
        )),
        "{comments:#?}"
    );

    let run_id = fixture.input["job_run_id"].as_str().unwrap();
    RuntimeHost::mark_job_run_running(&fixture.runtime, run_id, Utc::now(), std::process::id())
        .unwrap();
    RuntimeHost::finalize_job_run(
        &fixture.runtime,
        run_id,
        JobRunState::Failed,
        Utc::now(),
        None,
    )
    .unwrap();
    assert_eq!(
        status(&fixture),
        TaskStatus::Review,
        "the failed run leaves the reviewed PR's task in review"
    );
}

/// `pr_open` published PR #42 and `pr_promote` moved the task to review.
pub(super) fn published(fixture: &Fixture) {
    fixture
        .runtime
        .apply_task_automation_update(
            &fixture.task_id,
            TaskAutomationUpdate {
                status: Some(TaskStatus::Review),
                external_refs: vec![
                    ExternalRef::try_new(
                        GITHUB_PR_EXTERNAL_REF_SYSTEM.to_string(),
                        "42".to_string(),
                        None,
                    )
                    .unwrap(),
                ],
                ..TaskAutomationUpdate::default()
            },
        )
        .unwrap();
}

fn status(fixture: &Fixture) -> TaskStatus {
    fixture.runtime.get_task(&fixture.task_id).unwrap().status
}

fn failure_handoff(fixture: &Fixture, failed_step: &str, message: &str) -> Value {
    failure_handoff_after(fixture, failed_step, message, &json!({}))
}

/// The failure handoff with `steps` as the pipeline's earlier step outputs.
fn failure_handoff_after(
    fixture: &Fixture,
    failed_step: &str,
    message: &str,
    steps: &Value,
) -> Value {
    let mut pipeline = json!({
        "worktree": {"job_run_id": fixture.input["job_run_id"], "workspace_path": fixture.repo},
        "landing_review_gate_admit": fixture.input["admission"],
    });
    pipeline
        .as_object_mut()
        .unwrap()
        .extend(steps.as_object().unwrap().clone());
    execute_deterministic_action(
        &fixture.runtime,
        "pr_failure_handoff",
        &json!({}),
        &json!({
            "failed_step_id": failed_step,
            "error_code": "deterministic_action_refused",
            "error_message": message,
            "run_id": fixture.input["job_run_id"],
            "job_input": {"task_ids": [fixture.task_id], "base_branch": "main", "base_sync": "local"},
            "pipeline": pipeline,
        }),
        false,
        &Default::default(),
        None,
    )
    .unwrap()
}

/// The ledger the fixture's admitted attempt reserved in, and whether its
/// candidate already had its one review.
fn candidate_reviewed(fixture: &Fixture) -> (orbit_types::workflow::ReviewLedger, bool) {
    let ledger = fixture
        .runtime
        .review_store()
        .unwrap()
        .review_ledger(
            &fixture.runtime.workspace_id().unwrap(),
            fixture.input["admission"]["lineage_key"].as_str().unwrap(),
        )
        .unwrap()
        .unwrap();
    let attempt = ledger.attempts.last().unwrap();
    let reviewed = ledger.reviewed(&attempt.candidate, &attempt.task_meaning_digest);
    (ledger, reviewed)
}

fn report(fixture: &Fixture, summary: &str, validation: Value, escalation: Value) -> Value {
    json!({
        "schema_version": REVIEW_CONTRACT_VERSION,
        "attempt_id": fixture.input["admission"]["attempt_id"],
        "verdict": "incomplete", "summary": summary,
        "findings": [], "validation": validation, "escalation": escalation,
    })
}

/// A reviewer that exits cleanly with only the placeholder it persisted first
/// produced no verdict [ORB-15130]. Its attempt is released with a typed
/// refusal, so the candidate's one review stays unspent and the same run can
/// admit a fresh reviewer; the failure handoff states the open PR and the task
/// status the store holds.
#[test]
fn a_reviewer_that_leaves_only_its_initial_report_does_not_spend_the_review() {
    if !super::dispatch_admission::isolated(
        "review_before_landing::a_reviewer_that_leaves_only_its_initial_report_does_not_spend_the_review",
    ) {
        return;
    }
    let mut fixture = Fixture::before_landing();
    published(&fixture);
    fixture.input["before_landing"] = json!(true);
    fixture.admit();
    let first_attempt = fixture.input["admission"]["attempt_id"].clone();
    fixture.put_report(&report(
        &fixture,
        "Review still running; validation not yet complete.",
        json!([]),
        Value::Null,
    ));

    let refused = fixture
        .settle()
        .expect_err("an abandoned review is not an approval")
        .to_string();
    assert!(
        refused.contains(orbit_types::workflow::REVIEW_ABANDONED_MARKER)
            && refused.contains("stays open and unmerged"),
        "{refused}"
    );
    let (ledger, reviewed) = candidate_reviewed(&fixture);
    assert!(
        ledger.attempts[0].released_at.is_some(),
        "the abandoned attempt is released, not settled"
    );
    assert!(
        !reviewed,
        "an abandoned attempt is not the candidate's review"
    );
    assert!(
        fixture
            .runtime
            .get_task_artifact(
                &fixture.task_id,
                orbit_types::workflow::REVIEW_GATE_ARTIFACT
            )
            .unwrap()
            .is_none(),
        "no certificate records a verdict nobody reached"
    );
    assert_eq!(status(&fixture), TaskStatus::Review);

    let handoff = failure_handoff(&fixture, "landing_review_gate_settle", &refused);
    assert_eq!(handoff["decision"], "landing_review_failure", "{handoff}");
    assert_eq!(handoff["reason"], "review_abandoned", "{handoff}");

    // The same run admits a reviewer again: the candidate's review is not
    // spent, so the released attempt resumes instead of being refused.
    fixture.admit();
    assert_eq!(fixture.input["admission"]["applies"], true);
    assert_eq!(fixture.input["admission"]["attempt_id"], first_attempt);
    fixture.put_report(&report(
        &fixture,
        "Checked the published PR.",
        json!([]),
        json!("The change is wrong."),
    ));
    fixture
        .settle()
        .expect_err("a genuine incomplete stops delivery");
    assert!(
        candidate_reviewed(&fixture).1,
        "the resumed attempt's own verdict is the candidate's review"
    );
    let comments = fixture.runtime.get_task_comments(&fixture.task_id).unwrap();
    let verdict = comments
        .iter()
        .rfind(|comment| {
            comment
                .message
                .starts_with("before-landing review settled attempt")
        })
        .unwrap_or_else(|| panic!("{comments:#?}"));
    assert!(
        verdict.message.contains("PR #42 stays open and unmerged")
            && verdict.message.contains("the task stays `review`")
            && !verdict.message.contains("no PR is opened")
            && !verdict.message.contains("the task is blocked"),
        "{}",
        verdict.message
    );
}

/// Anything the reviewer decided or revised is a real review, however
/// incomplete: a reason, a recorded check or a revision past the first
/// settles the attempt and spends the candidate's review as before.
#[test]
fn an_incomplete_review_with_a_reason_a_record_or_a_revision_still_counts() {
    if !super::dispatch_admission::isolated(
        "review_before_landing::an_incomplete_review_with_a_reason_a_record_or_a_revision_still_counts",
    ) {
        return;
    }
    let record = json!([{
        "id": "V1", "command": "fixture check", "outcome": "passed", "role": "required",
    }]);
    for case in ["escalation", "validation record", "second revision"] {
        let mut fixture = Fixture::before_landing();
        published(&fixture);
        fixture.input["before_landing"] = json!(true);
        fixture.admit();
        let placeholder = "Review still running; validation not yet complete.";
        match case {
            "escalation" => fixture.put_report(&report(
                &fixture,
                placeholder,
                json!([]),
                json!("Missing evidence."),
            )),
            "validation record" => {
                fixture.put_report(&report(&fixture, placeholder, record.clone(), Value::Null));
            }
            _ => {
                fixture.put_report(&report(&fixture, placeholder, json!([]), Value::Null));
                fixture.put_report(&report(&fixture, "Still reading.", json!([]), Value::Null));
            }
        }
        let refused = fixture
            .settle()
            .expect_err("an incomplete review stops delivery")
            .to_string();
        assert!(refused.contains("review_gate_blocked"), "{case}: {refused}");
        let (ledger, reviewed) = candidate_reviewed(&fixture);
        assert!(reviewed, "{case}: a settled incomplete is the review");
        assert!(ledger.attempts[0].released_at.is_none(), "{case}");
    }
}

fn git(cwd: &Path, args: &[&str]) -> String {
    let mut command = Command::new("git");
    orbit_common::test_env::clear_inherited_authority(|key| {
        command.env_remove(key);
    });
    let output = command.args(args).current_dir(cwd).output().unwrap();
    assert!(
        output.status.success(),
        "git {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

/// A before-landing run whose candidate is published to a bare `origin`, whose
/// reviewer left an edit that settlement committed, and whose settlement was
/// then refused with `verdict`. The reviewer commit is local only.
struct Stranded {
    fixture: Fixture,
    bare: PathBuf,
    published: String,
    refused: String,
}

fn stranded(verdict: &str) -> Stranded {
    let mut fixture = Fixture::before_landing();
    let bare = fixture._root.path().join("origin.git");
    git(
        fixture._root.path(),
        &["init", "--bare", "-q", bare.to_str().unwrap()],
    );
    git(
        &fixture.repo,
        &["remote", "add", "origin", bare.to_str().unwrap()],
    );
    git(
        &fixture.repo,
        &["push", "-q", "origin", "main", "candidate"],
    );
    let published_head = git(&fixture.repo, &["rev-parse", "HEAD"]);

    published(&fixture);
    fixture.input["before_landing"] = json!(true);
    fixture.admit();
    std::fs::write(fixture.repo.join("candidate.txt"), "reviewed\n").unwrap();
    fixture.put_report(&json!({
        "schema_version": REVIEW_CONTRACT_VERSION,
        "attempt_id": fixture.input["admission"]["attempt_id"],
        "verdict": verdict, "summary": "Fixed what was found; one check is missing.",
        "findings": [],
        "validation": [{"id": "V1", "command": "fixture check", "outcome": "passed", "role": "required"}],
        "escalation": "A required check could not run.",
    }));
    let refused = fixture
        .settle()
        .expect_err("a review that does not approve is refused")
        .to_string();
    assert!(
        refused.contains(&format!("review_gate_blocked: verdict {verdict}")),
        "{refused}"
    );
    assert_ne!(
        git(&fixture.repo, &["rev-parse", "HEAD"]),
        published_head,
        "settlement committed the reviewer's edit"
    );
    assert_eq!(
        git(&bare, &["rev-parse", "refs/heads/candidate"]),
        published_head,
        "the reviewer commit is only in the run's worktree"
    );
    Stranded {
        fixture,
        bare,
        published: published_head,
        refused,
    }
}

impl Stranded {
    fn handoff(&self) -> Value {
        failure_handoff_after(
            &self.fixture,
            "landing_review_gate_settle",
            &self.refused,
            &json!({
                "sync_base": {"head": "candidate"},
                "push": {"local_sha": self.published},
            }),
        )
    }

    fn remote(&self, reference: &str) -> String {
        git(&self.bare, &["rev-parse", reference])
    }
}

/// The reviewer's fix commit settled `incomplete` is not left in the run's
/// worktree: the failure handoff pushes it onto the published head under a
/// lease, and the PR stays unmerged with the task in review [ORB-15329].
#[test]
fn an_incomplete_before_landing_review_pushes_its_reviewer_commit_and_merges_nothing() {
    if !super::dispatch_admission::isolated(
        "review_before_landing::an_incomplete_before_landing_review_pushes_its_reviewer_commit_and_merges_nothing",
    ) {
        return;
    }
    let stranded = stranded("incomplete");
    let main_before = stranded.remote("refs/heads/main");
    let reviewer_commit = git(&stranded.fixture.repo, &["rev-parse", "HEAD"]);

    let handoff = stranded.handoff();

    assert_eq!(handoff["decision"], "landing_review_failure", "{handoff}");
    assert_eq!(
        handoff["reviewer_push"]["decision"], "performed_fast_forward",
        "{handoff}"
    );
    assert_eq!(handoff["reviewer_push"]["local_sha"], reviewer_commit);
    assert_eq!(
        stranded.remote("refs/heads/candidate"),
        reviewer_commit,
        "the reviewer commit is on the remote PR branch"
    );
    assert_eq!(
        git(
            &stranded.bare,
            &["rev-parse", &format!("{reviewer_commit}^")]
        ),
        stranded.published,
        "it sits directly on the published head"
    );
    assert_eq!(stranded.remote("refs/heads/main"), main_before);
    assert_eq!(handoff["task_status"], "review", "{handoff}");
    assert_eq!(status(&stranded.fixture), TaskStatus::Review);
    let comments = stranded
        .fixture
        .runtime
        .get_task_comments(&stranded.fixture.task_id)
        .unwrap();
    assert!(
        comments.iter().any(|comment| {
            comment
                .message
                .starts_with("Before-landing review did not approve PR #42")
                && comment.message.contains("was pushed to the PR branch")
                && comment.message.contains("neither approved nor revalidated")
        }),
        "{comments:#?}"
    );
}

/// The push is conditional on the head the review reviewed: a branch that moved
/// since keeps its new head, the reviewer commit stays local, and the handoff
/// still leaves the PR open in review.
#[test]
fn a_reviewer_commit_is_not_pushed_over_a_pr_branch_that_moved() {
    if !super::dispatch_admission::isolated(
        "review_before_landing::a_reviewer_commit_is_not_pushed_over_a_pr_branch_that_moved",
    ) {
        return;
    }
    let stranded = stranded("incomplete");
    let other = stranded.fixture._root.path().join("other");
    git(
        stranded.fixture._root.path(),
        &[
            "clone",
            "-q",
            "-b",
            "candidate",
            stranded.bare.to_str().unwrap(),
            other.to_str().unwrap(),
        ],
    );
    git(&other, &["config", "user.name", "Orbit Test"]);
    git(&other, &["config", "user.email", "orbit-test@example.com"]);
    git(&other, &["config", "commit.gpgsign", "false"]);
    std::fs::write(other.join("other.txt"), "moved\n").unwrap();
    git(&other, &["add", "."]);
    git(&other, &["commit", "-q", "-m", "moved on"]);
    git(&other, &["push", "-q", "origin", "candidate"]);
    let moved = stranded.remote("refs/heads/candidate");

    let handoff = stranded.handoff();

    assert_eq!(handoff["decision"], "landing_review_failure", "{handoff}");
    assert_eq!(
        handoff["reviewer_push"]["decision"], "push_failed",
        "{handoff}"
    );
    assert!(
        handoff["reviewer_push"]["error"]
            .as_str()
            .unwrap()
            .contains("push_lease_lost"),
        "{handoff}"
    );
    assert_eq!(stranded.remote("refs/heads/candidate"), moved);
    assert_eq!(status(&stranded.fixture), TaskStatus::Review);
}

/// Only an `incomplete` settlement pushes: a `reject` leaves the reviewer
/// commit local like any other non-approving outcome.
#[test]
fn a_rejected_before_landing_review_pushes_nothing() {
    if !super::dispatch_admission::isolated(
        "review_before_landing::a_rejected_before_landing_review_pushes_nothing",
    ) {
        return;
    }
    let stranded = stranded("reject");

    let handoff = stranded.handoff();

    assert_eq!(handoff["decision"], "landing_review_failure", "{handoff}");
    assert_eq!(handoff["reviewer_push"], Value::Null, "{handoff}");
    assert_eq!(stranded.remote("refs/heads/candidate"), stranded.published);
    assert_eq!(status(&stranded.fixture), TaskStatus::Review);
}
