//! A run that captured `review.before_landing` reviews its published PR, not
//! its unpublished candidate, and any outcome but an approve keeps that PR
//! open and unmerged with the task in review [ORB-14849].

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use orbit_core::OrbitError;
use orbit_core::TaskStatus;
use orbit_core::runtime::task_pr_forge::{ForgePullRequest, MergedPullRequest, TaskPrForge};
use orbit_engine::{Dispatch, Recovery, RunLifecycle};
use orbit_engine::{TaskAutomationUpdate, execute_deterministic_action};
use orbit_types::task::{ExternalRef, GITHUB_PR_EXTERNAL_REF_SYSTEM};
use orbit_types::workflow::{JobRunState, REVIEW_CONTRACT_VERSION};
use serde_json::{Value, json};

use super::review_gate_audit::Fixture;

fn an_owner_before_landing_review_keeps_the_durable_task_summary() {
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
            &std::collections::BTreeMap::new(),
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
fn a_before_landing_review_that_does_not_approve_keeps_the_pr_open_in_review() {
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
    RunLifecycle::mark_job_run_running(&fixture.runtime, run_id, Utc::now(), std::process::id())
        .unwrap();
    RunLifecycle::finalize_job_run(
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
        .sqlite_store()
        .unwrap()
        .read()
        .and_then(|snapshot| {
            snapshot.review().ledger(
                &fixture.runtime.workspace_id().unwrap(),
                fixture.input["admission"]["lineage_key"].as_str().unwrap(),
            )
        })
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
fn a_reviewer_that_leaves_only_its_initial_report_does_not_spend_the_review() {
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
fn an_incomplete_review_with_a_reason_a_record_or_a_revision_still_counts() {
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
    let mut command = crate::git_test_support::command();
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

/// A forge on which the reviewed PR #42 already merged as [`EXTERNAL_MERGE`].
struct MergedForge;

const EXTERNAL_MERGE: &str = "94016e8238aa0cb5e1b3c9e4d2f7a6b8c0d1e2f3";

impl TaskPrForge for MergedForge {
    fn open_pull_requests(&self, _: &Path) -> Result<Vec<ForgePullRequest>, OrbitError> {
        Ok(Vec::new())
    }
    fn merged_pull_request(
        &self,
        _: &Path,
        number: u64,
    ) -> Result<Option<MergedPullRequest>, OrbitError> {
        Ok((number == 42).then(|| MergedPullRequest {
            pull: ForgePullRequest {
                number,
                head_branch: "candidate".into(),
                body: String::new(),
                author: "orbit-bot".into(),
            },
            url: "https://github.com/owner/repo/pull/42".into(),
            base_branch: "main".into(),
            head_sha: "0123456789abcdef0123456789abcdef01234567".into(),
            merge_commit: EXTERNAL_MERGE.into(),
            merged_at: Utc::now(),
        }))
    }
    fn authenticated_login(&self, _: &Path) -> Result<String, OrbitError> {
        Ok("orbit-bot".into())
    }
    fn close_pull_request(&self, _: &Path, _: u64, _: &str) -> Result<(), OrbitError> {
        Ok(())
    }
}

fn stranded(verdict: &str) -> Stranded {
    stranded_on(verdict, Fixture::before_landing())
}

fn stranded_on(verdict: &str, mut fixture: Fixture) -> Stranded {
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
fn an_incomplete_before_landing_review_pushes_its_reviewer_commit_and_merges_nothing() {
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

/// The PR merged outside Orbit while the review ran [ORB-26599]: the settle
/// comment and the refusal name the merge, claim no open or unmerged PR, name
/// the unpushed reviewer commit and its worktree, and leave no pending
/// decision; the failure handoff pushes nothing.
fn a_before_landing_review_of_a_pr_merged_outside_orbit_says_so_and_names_the_stranded_commit() {
    let mut fixture = Fixture::before_landing();
    fixture.runtime = fixture
        .runtime
        .clone()
        .with_task_pr_forge(Arc::new(MergedForge));
    let stranded = stranded_on("incomplete", fixture);
    let reviewer_commit = git(&stranded.fixture.repo, &["rev-parse", "HEAD"]);

    assert!(
        stranded.refused.contains(EXTERNAL_MERGE)
            && !stranded.refused.contains("stays open and unmerged")
            && orbit_types::workflow::HeldFailure::of(None, Some(&stranded.refused)).is_none(),
        "{}",
        stranded.refused
    );
    let comments = stranded
        .fixture
        .runtime
        .get_task_comments(&stranded.fixture.task_id)
        .unwrap();
    let verdict = comments
        .iter()
        .rfind(|comment| {
            comment
                .message
                .starts_with("before-landing review settled attempt")
        })
        .unwrap_or_else(|| panic!("{comments:#?}"));
    assert!(
        verdict
            .message
            .contains("PR #42 already merged outside Orbit")
            && verdict.message.contains(EXTERNAL_MERGE)
            && verdict.message.contains(&reviewer_commit)
            && verdict
                .message
                .contains(&stranded.fixture.repo.display().to_string())
            && !verdict.message.contains("stays open and unmerged")
            && !verdict.message.contains("An operator decides"),
        "{}",
        verdict.message
    );

    let handoff = stranded.handoff();
    assert_eq!(handoff["decision"], "landing_review_failure", "{handoff}");
    assert_eq!(handoff["reviewer_push"], Value::Null, "{handoff}");
    assert_eq!(stranded.remote("refs/heads/candidate"), stranded.published);
    let comments = stranded
        .fixture
        .runtime
        .get_task_comments(&stranded.fixture.task_id)
        .unwrap();
    let handoff_note = comments
        .iter()
        .rfind(|comment| comment.message.starts_with("Before-landing review ended"))
        .unwrap_or_else(|| panic!("{comments:#?}"));
    assert!(
        handoff_note
            .message
            .contains("already merged outside Orbit")
            && !handoff_note.message.contains("stays open and unmerged"),
        "{}",
        handoff_note.message
    );
}

/// An accepted reviewer commit on a PR that merged outside Orbit still reaches
/// the push step, which may preserve it and block the task with a recipe: the
/// settle comment must not call it worktree-only or decision-free.
fn an_accepted_reviewer_fix_on_a_pr_merged_outside_orbit_defers_to_the_push_step() {
    let mut fixture = Fixture::before_landing();
    fixture.runtime = fixture
        .runtime
        .clone()
        .with_task_pr_forge(Arc::new(MergedForge));
    fixture.input["before_landing"] = json!(true);
    published(&fixture);
    fixture.admit();
    std::fs::write(fixture.repo.join("candidate.txt"), "reviewed\n").unwrap();
    fixture.put_report(&json!({
        "schema_version": REVIEW_CONTRACT_VERSION,
        "attempt_id": fixture.input["admission"]["attempt_id"],
        "verdict": "accept_with_fixes", "summary": "Fixed what was found.",
        "findings": [{
            "id": "F1", "severity": "low", "summary": "Line missing",
            "paths": ["candidate.txt"], "disposition": {"kind": "repaired"},
            "change": "Added the line",
        }],
        "validation": [{"id": "V1", "command": "fixture check", "outcome": "passed", "role": "required"}],
    }));
    fixture.settle().expect("an accepted review settles");

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
        verdict
            .message
            .contains("PR #42 already merged outside Orbit")
            && verdict.message.contains("durable ref")
            && !verdict.message.contains("no decision is pending")
            && !verdict.message.contains("was not pushed"),
        "{}",
        verdict.message
    );
}

/// The push is conditional on the head the review reviewed: a branch that moved
/// since keeps its new head, the reviewer commit is preserved on a durable ref
/// instead, and the handoff reports both the refusal and that ref while still
/// leaving the PR open in review.
fn a_reviewer_commit_is_not_pushed_over_a_pr_branch_that_moved() {
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
    let reviewer_commit = git(&stranded.fixture.repo, &["rev-parse", "HEAD"]);
    let run_id = stranded.fixture.input["job_run_id"].as_str().unwrap();
    let expected_ref = format!(
        "refs/orbit/candidates/{}/{run_id}",
        stranded.fixture.task_id
    );

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
    assert_eq!(
        handoff["reviewer_preservation"]["decision"], "preserved",
        "{handoff}"
    );
    assert_eq!(
        handoff["reviewer_preservation"]["commit_sha"], reviewer_commit,
        "{handoff}"
    );
    assert_eq!(
        handoff["reviewer_preservation"]["ref"], expected_ref,
        "{handoff}"
    );
    assert_eq!(stranded.remote("refs/heads/candidate"), moved);
    assert_eq!(stranded.remote(&expected_ref), reviewer_commit);
    assert_eq!(status(&stranded.fixture), TaskStatus::Review);
    let comments = stranded
        .fixture
        .runtime
        .get_task_comments(&stranded.fixture.task_id)
        .unwrap();
    assert!(
        comments.iter().any(|comment| {
            comment.message.contains("push_lease_lost")
                && comment.message.contains("PR branch is unchanged")
                && comment.message.contains(&reviewer_commit)
                && comment.message.contains(&expected_ref)
        }),
        "{comments:#?}"
    );
}

/// Only an `incomplete` settlement pushes to the PR branch: a `reject` leaves
/// the PR branch untouched at the published head, preserves any reviewer fix
/// commit on a durable ref, and names both the commit and ref in the delivery
/// comment.
fn a_rejected_before_landing_review_pushes_nothing() {
    let stranded = stranded("reject");
    let reviewer_commit = git(&stranded.fixture.repo, &["rev-parse", "HEAD"]);
    let run_id = stranded.fixture.input["job_run_id"].as_str().unwrap();
    let expected_ref = format!(
        "refs/orbit/candidates/{}/{run_id}",
        stranded.fixture.task_id
    );

    let handoff = stranded.handoff();

    assert_eq!(handoff["decision"], "landing_review_failure", "{handoff}");
    assert_eq!(handoff["reviewer_push"], Value::Null, "{handoff}");
    assert_eq!(
        handoff["reviewer_preservation"]["decision"], "preserved",
        "{handoff}"
    );
    assert_eq!(
        handoff["reviewer_preservation"]["commit_sha"], reviewer_commit,
        "{handoff}"
    );
    assert_eq!(
        handoff["reviewer_preservation"]["ref"], expected_ref,
        "{handoff}"
    );
    assert_eq!(stranded.remote("refs/heads/candidate"), stranded.published);
    assert_eq!(stranded.remote(&expected_ref), reviewer_commit);
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
                && comment.message.contains(&reviewer_commit)
                && comment.message.contains(&expected_ref)
                && comment.message.contains("not pushed to the PR branch")
        }),
        "{comments:#?}"
    );
}

fn setup_before_landing_remote() -> (Fixture, PathBuf, String) {
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
    (fixture, bare, published_head)
}

/// A before-landing review that times out mid-repair commits its uncommitted
/// repairs, preserves that commit on a durable candidate ref (never pushing to
/// the PR branch), and names the commit and ref in the delivery comment.
fn a_timed_out_before_landing_review_preserves_uncommitted_repairs_on_a_durable_ref() {
    let (fixture, bare, published_head) = setup_before_landing_remote();
    std::fs::write(
        fixture.repo.join("candidate.txt"),
        "timed out reviewer repair\n",
    )
    .unwrap();
    let run_id = fixture.input["job_run_id"].as_str().unwrap();
    let expected_ref = format!("refs/orbit/candidates/{}/{run_id}", fixture.task_id);

    let handoff = failure_handoff_after(
        &fixture,
        "landing_review",
        "review_timeout_incomplete: timed out mid-repair",
        &json!({
            "sync_base": {"head": "candidate"},
            "push": {"local_sha": published_head},
        }),
    );

    assert_eq!(handoff["decision"], "landing_review_failure", "{handoff}");
    assert_eq!(handoff["reviewer_push"], Value::Null, "{handoff}");
    assert_eq!(
        handoff["reviewer_preservation"]["decision"], "preserved",
        "{handoff}"
    );
    assert_eq!(
        handoff["reviewer_preservation"]["committed_by_handoff"], true,
        "{handoff}"
    );
    let preserved_commit = handoff["reviewer_preservation"]["commit_sha"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(preserved_commit, published_head);
    assert_eq!(handoff["reviewer_preservation"]["ref"], expected_ref);
    assert_eq!(
        git(&bare, &["rev-parse", "refs/heads/candidate"]),
        published_head
    );
    assert_eq!(git(&bare, &["rev-parse", &expected_ref]), preserved_commit);
    assert_eq!(
        git(&bare, &["rev-parse", &format!("{preserved_commit}^")]),
        published_head
    );
    let comments = fixture.runtime.get_task_comments(&fixture.task_id).unwrap();
    assert!(
        comments.iter().any(|comment| {
            comment.message.contains(&preserved_commit) && comment.message.contains(&expected_ref)
        }),
        "{comments:#?}"
    );
}

/// When a timed-out landing review's uncommitted repair cannot be committed,
/// the handoff captures the uncommitted diff in a task artifact and names both
/// the refusal reason and where the diff was saved.
fn a_timed_out_before_landing_review_saves_uncommittable_diff_as_artifact_and_names_refusal() {
    let (fixture, bare, published_head) = setup_before_landing_remote();
    let left_path = fixture.repo.join("left.tmp");
    let right_path = fixture.repo.join("right.tmp");
    std::fs::write(&left_path, "ours\n").unwrap();
    std::fs::write(&right_path, "theirs\n").unwrap();
    let left_sha = git(
        &fixture.repo,
        &["hash-object", "-w", left_path.to_str().unwrap()],
    );
    let right_sha = git(
        &fixture.repo,
        &["hash-object", "-w", right_path.to_str().unwrap()],
    );
    std::fs::remove_file(&left_path).unwrap();
    std::fs::remove_file(&right_path).unwrap();
    let index_input =
        format!("100644 {left_sha} 2\tconflict.txt\n100644 {right_sha} 3\tconflict.txt\n");
    let mut child = std::process::Command::new("git")
        .args(["update-index", "--index-info"])
        .current_dir(&fixture.repo)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(index_input.as_bytes())
        .unwrap();
    assert!(child.wait().unwrap().success());
    std::fs::write(
        fixture.repo.join("conflict.txt"),
        "resolved locally\nsk-syntheticlandingcanary000000\n",
    )
    .unwrap();
    std::fs::write(
        fixture.repo.join(".env"),
        "ENV_CANARY=synthetic_landing_env_canary\n",
    )
    .unwrap();

    let run_id = fixture.input["job_run_id"].as_str().unwrap();
    let expected_artifact = format!("landing-review-uncommitted-{run_id}.patch");
    let handoff = failure_handoff_after(
        &fixture,
        "landing_review",
        "review_timeout_incomplete: timed out mid-repair",
        &json!({
            "sync_base": {"head": "candidate"},
            "push": {"local_sha": published_head},
        }),
    );

    assert_eq!(handoff["decision"], "landing_review_failure", "{handoff}");
    assert_eq!(handoff["reviewer_push"], Value::Null, "{handoff}");
    assert_eq!(
        handoff["reviewer_preservation"]["decision"], "commit_refused",
        "{handoff}"
    );
    assert_eq!(
        handoff["reviewer_preservation"]["diff_artifact"], expected_artifact,
        "{handoff}"
    );
    let reason = handoff["reviewer_preservation"]["error"].as_str().unwrap();
    assert!(!reason.is_empty());
    assert_eq!(
        git(&bare, &["rev-parse", "refs/heads/candidate"]),
        published_head
    );

    let artifact = fixture
        .runtime
        .get_task_artifact(&fixture.task_id, &expected_artifact)
        .unwrap()
        .expect("uncommitted diff artifact is saved on the task");
    let diff_text = String::from_utf8_lossy(&artifact.content);
    assert!(diff_text.contains("resolved locally"), "{diff_text}");
    assert!(
        !diff_text.contains(".env") && !diff_text.contains("synthetic_landing_env_canary"),
        "protected env file leaked into landing-review patch: {diff_text}"
    );
    assert!(
        !diff_text.contains("sk-syntheticlandingcanary000000")
            && diff_text.contains("[REDACTED_SECRET]"),
        "unredacted credential canary leaked into landing-review patch: {diff_text}"
    );
    let env_blob = git(&fixture.repo, &["hash-object", ".env"]);
    let exists = crate::git_test_support::command()
        .args(["cat-file", "-e", &env_blob])
        .current_dir(&fixture.repo)
        .output()
        .unwrap()
        .status
        .success();
    assert!(
        !exists,
        "protected .env blob {env_blob} must not enter repository object store"
    );

    let comments = fixture.runtime.get_task_comments(&fixture.task_id).unwrap();
    assert!(
        comments.iter().any(|comment| {
            comment.message.contains(&expected_artifact) && comment.message.contains(reason)
        }),
        "{comments:#?}"
    );
}

fn failed_reviewer_commit_preservation_excludes_protected_changes_and_deletions() {
    let (fixture, _bare, _) = setup_before_landing_remote();
    std::fs::write(fixture.repo.join(".env"), "old_private_value\n").unwrap();
    std::fs::write(fixture.repo.join(".env.deleted"), "deleted_private_value\n").unwrap();
    git(&fixture.repo, &["add", "-f", ".env", ".env.deleted"]);
    git(
        &fixture.repo,
        &["commit", "-q", "-m", "published protected files"],
    );
    let published_head = git(&fixture.repo, &["rev-parse", "HEAD"]);
    std::fs::write(fixture.repo.join(".env"), "new_private_value\n").unwrap();
    std::fs::remove_file(fixture.repo.join(".env.deleted")).unwrap();
    std::fs::write(
        fixture.repo.join("candidate.txt"),
        "eligible repair\nsk-syntheticlandingcanary000000\n",
    )
    .unwrap();
    git(&fixture.repo, &["add", "-u"]);
    git(&fixture.repo, &["commit", "-q", "-m", "reviewer repair"]);
    git(&fixture.repo, &["remote", "remove", "origin"]);

    let run_id = fixture.input["job_run_id"].as_str().unwrap();
    let artifact_path = format!("landing-review-fix-{run_id}.patch");
    let handoff = failure_handoff_after(
        &fixture,
        "landing_review_gate_settle",
        "review_gate_blocked: verdict reject",
        &json!({
            "sync_base": {"head": "candidate"}, "push": {"local_sha": published_head},
        }),
    );
    assert_eq!(
        handoff["reviewer_preservation"]["decision"], "preserve_failed",
        "{handoff}"
    );
    assert_eq!(
        handoff["reviewer_preservation"]["diff_artifact"], artifact_path,
        "{handoff}"
    );
    let artifact = fixture
        .runtime
        .get_task_artifact(&fixture.task_id, &artifact_path)
        .unwrap()
        .unwrap();
    let diff = String::from_utf8_lossy(&artifact.content);
    assert!(
        diff.contains("eligible repair") && diff.contains("[REDACTED_SECRET]"),
        "{diff}"
    );
    for forbidden in [
        ".env",
        "old_private_value",
        "new_private_value",
        "deleted_private_value",
        "sk-syntheticlandingcanary000000",
    ] {
        assert!(!diff.contains(forbidden), "leaked {forbidden}: {diff}");
    }
}

/// Runs the failed-preservation fallback for a reviewer commit on top of
/// `published_head` and returns the `landing-review-fix` patch it saved.
fn failed_preservation_patch(fixture: &Fixture, published_head: &str) -> String {
    let handoff = failure_handoff_after(
        fixture,
        "landing_review_gate_settle",
        "review_gate_blocked: verdict reject",
        &json!({
            "sync_base": {"head": "candidate"}, "push": {"local_sha": published_head},
        }),
    );
    let run_id = fixture.input["job_run_id"].as_str().unwrap();
    let artifact_path = format!("landing-review-fix-{run_id}.patch");
    assert_eq!(
        handoff["reviewer_preservation"]["decision"], "preserve_failed",
        "{handoff}"
    );
    assert_eq!(
        handoff["reviewer_preservation"]["diff_artifact"], artifact_path,
        "{handoff}"
    );
    let artifact = fixture
        .runtime
        .get_task_artifact(&fixture.task_id, &artifact_path)
        .unwrap()
        .unwrap();
    String::from_utf8_lossy(&artifact.content).into_owned()
}

/// A reviewer commit that replaces a published protected directory with an
/// ordinary file of the same name. A literal pathspec for `d` also matches the
/// published `d/.env`, so the fallback must exclude that descendant.
fn failed_reviewer_commit_preservation_excludes_protected_descendants_when_a_file_replaces_a_directory()
 {
    let (fixture, _bare, _) = setup_before_landing_remote();
    std::fs::create_dir(fixture.repo.join("d")).unwrap();
    std::fs::write(fixture.repo.join("d/.env"), "old_private_value\n").unwrap();
    git(&fixture.repo, &["add", "-f", "d"]);
    git(
        &fixture.repo,
        &["commit", "-q", "-m", "published protected directory"],
    );
    let published_head = git(&fixture.repo, &["rev-parse", "HEAD"]);
    git(&fixture.repo, &["rm", "-q", "-r", "d"]);
    std::fs::write(fixture.repo.join("d"), "ordinary replacement\n").unwrap();
    git(&fixture.repo, &["add", "d"]);
    git(
        &fixture.repo,
        &["commit", "-q", "-m", "reviewer replaces directory"],
    );
    git(&fixture.repo, &["remote", "remove", "origin"]);

    let diff = failed_preservation_patch(&fixture, &published_head);
    assert!(diff.contains("ordinary replacement"), "{diff}");
    for forbidden in [".env", "old_private_value"] {
        assert!(!diff.contains(forbidden), "leaked {forbidden}: {diff}");
    }
}

/// A reviewer commit that replaces a published ordinary file with a directory
/// holding a protected file. The published `d` is a literal pathspec match for
/// the new `d/.env`, so the fallback must exclude that protected addition.
fn failed_reviewer_commit_preservation_excludes_protected_descendants_when_a_directory_replaces_a_file()
 {
    let (fixture, _bare, _) = setup_before_landing_remote();
    std::fs::write(fixture.repo.join("d"), "ordinary published file\n").unwrap();
    git(&fixture.repo, &["add", "d"]);
    git(
        &fixture.repo,
        &["commit", "-q", "-m", "published ordinary file"],
    );
    let published_head = git(&fixture.repo, &["rev-parse", "HEAD"]);
    git(&fixture.repo, &["rm", "-q", "d"]);
    std::fs::create_dir(fixture.repo.join("d")).unwrap();
    std::fs::write(fixture.repo.join("d/.env"), "new_private_value\n").unwrap();
    std::fs::write(fixture.repo.join("d/notes.txt"), "ordinary nested note\n").unwrap();
    git(&fixture.repo, &["add", "-f", "d"]);
    git(
        &fixture.repo,
        &["commit", "-q", "-m", "reviewer replaces file"],
    );
    git(&fixture.repo, &["remote", "remove", "origin"]);

    let diff = failed_preservation_patch(&fixture, &published_head);
    assert!(
        diff.contains("ordinary nested note") && diff.contains("ordinary published file"),
        "{diff}"
    );
    for forbidden in [".env", "new_private_value"] {
        assert!(!diff.contains(forbidden), "leaked {forbidden}: {diff}");
    }
}

/// Clone `source_repo`, check out `base_rev`, apply `patch`, and return the
/// temporary root whose `checkout` subdirectory holds the patched files.
fn apply_patch_in_fresh_checkout(
    source_repo: &Path,
    base_rev: &str,
    patch: &[u8],
) -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    let checkout = root.path().join("checkout");
    git(
        root.path(),
        &[
            "clone",
            "-q",
            source_repo.to_str().unwrap(),
            checkout.to_str().unwrap(),
        ],
    );
    git(&checkout, &["checkout", "-q", base_rev]);
    let patch_file = root.path().join("recovery.patch");
    std::fs::write(&patch_file, patch).unwrap();
    let output = crate::git_test_support::command()
        .args(["apply", patch_file.to_str().unwrap()])
        .current_dir(&checkout)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git apply failed ({}):\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    root
}

fn refused_reviewer_commit_patch_reproduces_tracked_binary_and_final_whitespace() {
    let mut fixture = Fixture::before_landing();
    let tracked_before = b"\x00\x01\x02\x03\xff\n".to_vec();
    let raw_before = b"hdr\xfe\xfd\n".to_vec();
    let trailing_before = b"first line\nsecond line\n".to_vec();
    let no_nl_before = b"seed\n".to_vec();
    std::fs::write(fixture.repo.join("tracked.bin"), &tracked_before).unwrap();
    std::fs::write(fixture.repo.join("raw.dat"), &raw_before).unwrap();
    std::fs::write(fixture.repo.join("trailing.txt"), &trailing_before).unwrap();
    std::fs::write(fixture.repo.join("no_nl.txt"), &no_nl_before).unwrap();
    git(
        &fixture.repo,
        &["add", "tracked.bin", "raw.dat", "trailing.txt", "no_nl.txt"],
    );
    git(
        &fixture.repo,
        &["commit", "-q", "-m", "seed binary and text"],
    );

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

    let tracked_after = b"\x00\x89PNG\r\n\x1a\n\x00\xff\xfe\x04".to_vec();
    let raw_after = b"hdr\x80\x81\xff\n".to_vec();
    let trailing_after = b"first line\nfinal spaces   \n".to_vec();
    let no_nl_after = b"no newline   ".to_vec();
    let added_bin = b"\x7fELF\x00\x01\x02\xff".to_vec();
    std::fs::write(fixture.repo.join("tracked.bin"), &tracked_after).unwrap();
    std::fs::write(fixture.repo.join("raw.dat"), &raw_after).unwrap();
    std::fs::write(fixture.repo.join("trailing.txt"), &trailing_after).unwrap();
    std::fs::write(fixture.repo.join("no_nl.txt"), &no_nl_after).unwrap();
    std::fs::write(fixture.repo.join("added.bin"), &added_bin).unwrap();
    // Detach HEAD to force commit_reviewer_repairs refusal while keeping
    // tracked ordinary edits and an untracked protected .env in the worktree.
    git(&fixture.repo, &["checkout", "--detach", "-q", "HEAD"]);
    std::fs::write(fixture.repo.join(".env"), "ENV_CANARY=refused_commit\n").unwrap();

    let run_id = fixture.input["job_run_id"].as_str().unwrap();
    let expected_artifact = format!("landing-review-uncommitted-{run_id}.patch");
    let handoff = failure_handoff_after(
        &fixture,
        "landing_review",
        "review_timeout_incomplete: timed out mid-repair",
        &json!({
            "sync_base": {"head": "candidate"},
            "push": {"local_sha": published_head},
        }),
    );
    assert_eq!(
        handoff["reviewer_preservation"]["decision"], "commit_refused",
        "{handoff}"
    );
    assert_eq!(
        handoff["reviewer_preservation"]["diff_artifact"], expected_artifact,
        "{handoff}"
    );

    let artifact = fixture
        .runtime
        .get_task_artifact(&fixture.task_id, &expected_artifact)
        .unwrap()
        .expect("uncommitted diff artifact is saved on the task");
    let applied_root = apply_patch_in_fresh_checkout(&bare, &published_head, &artifact.content);
    let applied = applied_root.path().join("checkout");
    assert_eq!(
        std::fs::read(applied.join("tracked.bin")).unwrap(),
        tracked_after
    );
    assert_eq!(std::fs::read(applied.join("raw.dat")).unwrap(), raw_after);
    assert_eq!(
        std::fs::read(applied.join("trailing.txt")).unwrap(),
        trailing_after
    );
    assert_eq!(
        std::fs::read(applied.join("no_nl.txt")).unwrap(),
        no_nl_after
    );
    assert_eq!(std::fs::read(applied.join("added.bin")).unwrap(), added_bin);
    assert!(!applied.join(".env").exists());
}

fn refused_durable_push_patch_reproduces_saved_commit_binary_changes() {
    let mut fixture = Fixture::before_landing();
    std::fs::write(fixture.repo.join("tracked.bin"), b"\x00\x01\x02\x03\xff\n").unwrap();
    std::fs::write(fixture.repo.join("raw.dat"), b"hdr\xfe\xfd\n").unwrap();
    std::fs::write(fixture.repo.join("trailing.txt"), b"first\nsecond\n").unwrap();
    git(
        &fixture.repo,
        &["add", "tracked.bin", "raw.dat", "trailing.txt"],
    );
    git(
        &fixture.repo,
        &["commit", "-q", "-m", "published binary base"],
    );
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

    let tracked_after = b"\x00\x89PNG\r\n\x1a\n\x00\xff\xfe\x04".to_vec();
    let raw_after = b"hdr\x80\x81\xff\n".to_vec();
    let trailing_after = b"first\nfinal spaces   \n".to_vec();
    let added_bin = b"\x7fELF\x00\x01\x02\xff".to_vec();
    std::fs::write(fixture.repo.join("tracked.bin"), &tracked_after).unwrap();
    std::fs::write(fixture.repo.join("raw.dat"), &raw_after).unwrap();
    std::fs::write(fixture.repo.join("trailing.txt"), &trailing_after).unwrap();
    std::fs::write(fixture.repo.join("added.bin"), &added_bin).unwrap();
    git(
        &fixture.repo,
        &["add", "tracked.bin", "raw.dat", "trailing.txt", "added.bin"],
    );
    git(
        &fixture.repo,
        &["commit", "-q", "-m", "reviewer binary commit"],
    );
    git(&fixture.repo, &["remote", "remove", "origin"]);

    let handoff = failure_handoff_after(
        &fixture,
        "landing_review_gate_settle",
        "review_gate_blocked: verdict reject",
        &json!({
            "sync_base": {"head": "candidate"},
            "push": {"local_sha": published_head},
        }),
    );
    let run_id = fixture.input["job_run_id"].as_str().unwrap();
    let artifact_path = format!("landing-review-fix-{run_id}.patch");
    assert_eq!(
        handoff["reviewer_preservation"]["decision"], "preserve_failed",
        "{handoff}"
    );
    assert_eq!(
        handoff["reviewer_preservation"]["diff_artifact"], artifact_path,
        "{handoff}"
    );

    let artifact = fixture
        .runtime
        .get_task_artifact(&fixture.task_id, &artifact_path)
        .unwrap()
        .unwrap();
    let applied_root =
        apply_patch_in_fresh_checkout(&fixture.repo, &published_head, &artifact.content);
    let applied = applied_root.path().join("checkout");
    assert_eq!(
        std::fs::read(applied.join("tracked.bin")).unwrap(),
        tracked_after
    );
    assert_eq!(std::fs::read(applied.join("raw.dat")).unwrap(), raw_after);
    assert_eq!(
        std::fs::read(applied.join("trailing.txt")).unwrap(),
        trailing_after
    );
    assert_eq!(std::fs::read(applied.join("added.bin")).unwrap(), added_bin);
}

// Related boundary scenarios share the isolated table fixture.
crate::scenario_table!(
    an_owner_before_landing_review_keeps_the_durable_task_summary_scenarios,
    an_owner_before_landing_review_keeps_the_durable_task_summary,
    a_before_landing_review_that_does_not_approve_keeps_the_pr_open_in_review,
    a_reviewer_that_leaves_only_its_initial_report_does_not_spend_the_review,
    an_incomplete_review_with_a_reason_a_record_or_a_revision_still_counts,
    an_incomplete_before_landing_review_pushes_its_reviewer_commit_and_merges_nothing,
    a_reviewer_commit_is_not_pushed_over_a_pr_branch_that_moved,
    a_before_landing_review_of_a_pr_merged_outside_orbit_says_so_and_names_the_stranded_commit,
    an_accepted_reviewer_fix_on_a_pr_merged_outside_orbit_defers_to_the_push_step,
    a_rejected_before_landing_review_pushes_nothing,
    a_timed_out_before_landing_review_preserves_uncommitted_repairs_on_a_durable_ref,
    a_timed_out_before_landing_review_saves_uncommittable_diff_as_artifact_and_names_refusal,
    failed_reviewer_commit_preservation_excludes_protected_changes_and_deletions,
    failed_reviewer_commit_preservation_excludes_protected_descendants_when_a_file_replaces_a_directory,
    failed_reviewer_commit_preservation_excludes_protected_descendants_when_a_directory_replaces_a_file,
    refused_reviewer_commit_patch_reproduces_tracked_binary_and_final_whitespace,
    refused_durable_push_patch_reproduces_saved_commit_binary_changes
);
