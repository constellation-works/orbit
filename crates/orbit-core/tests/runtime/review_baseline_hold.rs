//! [ORB-14434] A before-PR review whose only failure is a required check the
//! pinned base fails the same way holds the task for the red base instead of
//! blocking it, through the real settlement, failure handoff, admission and
//! candidate resume over a temp repository.

use std::collections::HashMap;
use std::path::Path;

use chrono::Utc;
use orbit_core::application::task::TaskUpdateParams;
use orbit_core::{TaskComplexity, TaskStatus};
use orbit_engine::{RuntimeHost, execute_deterministic_action};
use orbit_types::workflow::{
    BASELINE_RED_HOLD_EVENT, BaselineRedHold, PipelineState, REVIEW_GATE_ARTIFACT,
    ReviewCertificate, ReviewVerdict, is_baseline_red_failure,
};
use serde_json::{Value, json};

use super::review_gate_audit::Fixture;

/// The check the workspace requires beyond the host-required commands. It
/// fails `suite::broken` wherever `broken` is set, and `suite::regressed`
/// wherever the candidate's file says so. The red check also logs each
/// directory it ran in to [`CHECK_CWD_LOG`] in the Git common directory.
const CHECK: &str = "sh check.sh";
const CHECK_CWD_LOG: &str = "check-cwd.log";
const RED: &str = "#!/bin/sh\npwd -P >> \"$(git rev-parse --path-format=absolute --git-common-dir)/check-cwd.log\"\n\
                   echo 'test suite::broken ... FAILED'\n\
                   if grep -q regressed candidate.txt; then echo 'test suite::regressed ... FAILED'; fi\n\
                   exit 1\n";
const GREEN_ON_BASE: &str = "#!/bin/sh\n\
                             if grep -q after candidate.txt; then echo 'test suite::broken ... FAILED'; exit 1; fi\n\
                             exit 0\n";
const GREEN: &str = "#!/bin/sh\nexit 0\n";

pub(super) fn git(repo: &Path, args: &[&str]) -> String {
    let mut command = std::process::Command::new("git");
    orbit_common::test_env::clear_inherited_authority(|key| {
        command.env_remove(key);
    });
    let output = command.args(args).current_dir(repo).output().unwrap();
    assert!(output.status.success(), "git {args:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

/// A gated task whose base carries `check` as `check.sh`, with the
/// candidate (`after`, then `candidate` when that differs, in
/// `candidate.txt`) rebased onto it. Settlement may rerun [`CHECK`].
fn fixture(check: &str, candidate: &str) -> Fixture {
    let fixture = Fixture::new_with_config(&[], "baseline_commands = [\"sh check.sh\"]\n");
    let repo = &fixture.repo;
    git(repo, &["checkout", "--quiet", "main"]);
    std::fs::write(repo.join("check.sh"), check).unwrap();
    // A check the host does not trust: it marks that it ran.
    std::fs::write(
        repo.join("untrusted.sh"),
        "#!/bin/sh\ntouch .orbit/tmp/untrusted-ran\nexit 1\n",
    )
    .unwrap();
    git(repo, &["add", "check.sh", "untrusted.sh"]);
    git(repo, &["commit", "--quiet", "-m", "base adds its checks"]);
    git(repo, &["checkout", "--quiet", "candidate"]);
    git(repo, &["rebase", "--quiet", "main"]);
    if candidate != "after\n" {
        std::fs::write(repo.join("candidate.txt"), candidate).unwrap();
        git(repo, &["commit", "--quiet", "-am", "candidate change"]);
    }
    fixture
}

/// The reviewer's report: `command` failed on the candidate and, it claims,
/// on the pinned base the same way.
fn claim_report(fixture: &Fixture, command: &str, sources: &[&str]) -> Value {
    json!({
        "schema_version": 1,
        "attempt_id": fixture.input["admission"]["attempt_id"],
        "verdict": "reject",
        "summary": "The candidate is sound; the check fails on the base too.",
        "findings": [],
        "validation": [{
            "id": "V1", "command": command, "outcome": "failed", "role": "required",
            "sources": sources,
            "baseline": {
                "base_commit": fixture.input["admission"]["base_sha"],
                "outcome": "failed",
                "failures": ["suite::broken"],
            },
        }],
        "escalation": "The required check fails on the pinned base.",
    })
}

fn certificate(fixture: &Fixture) -> ReviewCertificate {
    serde_json::from_slice(
        &fixture
            .runtime
            .get_task_artifact(&fixture.task_id, REVIEW_GATE_ARTIFACT)
            .unwrap()
            .unwrap()
            .content,
    )
    .unwrap()
}

/// Settle a report claiming a red base and return the step's failure text.
fn settle_claim(fixture: &mut Fixture, command: &str, sources: &[&str]) -> String {
    fixture.admit();
    fixture.put_report(&claim_report(fixture, command, sources));
    fixture
        .settle()
        .expect_err("a failed required check never passes")
        .to_string()
}

/// The admission snapshot's entry for the fixture's task.
fn readiness(fixture: &Fixture) -> Value {
    let readiness = fixture
        .runtime
        .workspace_auto_readiness(&[], None, 50, &[])
        .unwrap();
    readiness["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["task_id"] == fixture.task_id.as_str())
        .cloned()
        .expect("task in readiness")
}

#[test]
fn a_check_failing_identically_on_the_pinned_base_holds_the_kept_candidate() {
    if !super::dispatch_admission::isolated(
        "review_baseline_hold::a_check_failing_identically_on_the_pinned_base_holds_the_kept_candidate",
    ) {
        return;
    }
    let mut fixture = fixture(RED, "after\n");
    let candidate = git(&fixture.repo, &["rev-parse", "HEAD"]);
    let failure = settle_claim(&mut fixture, CHECK, &["check.sh"]);
    let base_sha = fixture.input["admission"]["base_sha"]
        .as_str()
        .unwrap()
        .to_string();
    let run_id = fixture.input["job_run_id"].as_str().unwrap().to_string();

    assert!(
        is_baseline_red_failure(None, Some(&failure)),
        "the refusal is typed for the red base: {failure}"
    );
    // [ORB-14805] The base rerun ran in a checkout outside the Git common
    // directory: the Linux Git protection scan refuses every symlink under
    // it, so a base checkout there failed every sandboxed Git consumer.
    let common = fixture.repo.join(".git").canonicalize().unwrap();
    let ran_in = std::fs::read_to_string(common.join(CHECK_CWD_LOG)).unwrap();
    let repo = fixture.repo.canonicalize().unwrap();
    assert!(
        ran_in.lines().any(|cwd| Path::new(cwd) != repo),
        "the check reran on a base checkout: {ran_in}"
    );
    assert!(
        ran_in
            .lines()
            .all(|cwd| !Path::new(cwd).starts_with(&common)),
        "no check ran inside the Git common directory: {ran_in}"
    );
    let hold = BaselineRedHold::from_text(&failure).expect("the refusal names its hold");
    assert_eq!(
        hold,
        BaselineRedHold {
            base_ref: "main".into(),
            base_sha: base_sha.clone(),
            command: CHECK.into(),
            run_id: run_id.clone(),
        }
    );
    let settled = certificate(&fixture);
    assert_eq!(settled.verdict, ReviewVerdict::Reject, "the verdict stands");
    assert_eq!(settled.baseline_red, vec![hold.clone()]);
    assert_eq!(settled.final_candidate.commit, candidate);

    // The pipeline's failure handoff receives the typed refusal.
    let handoff = execute_deterministic_action(
        &fixture.runtime,
        "pr_failure_handoff",
        &json!({}),
        &json!({
            "failed_step_id": "review_gate_settle",
            "error_code": "deterministic_action_refused",
            "error_message": failure,
            "run_id": run_id,
            "job_input": {"task_ids": [fixture.task_id]},
            "pipeline": {"worktree": {
                "job_run_id": run_id, "workspace_path": fixture.input["workspace_path"],
            }},
        }),
        false,
        &HashMap::new(),
        None,
    )
    .unwrap();
    assert_eq!(handoff["decision"], "held_baseline_red", "{handoff}");
    assert_eq!(handoff["candidate_preserved"], true, "{handoff}");
    assert_eq!(handoff["pr_created"], false, "{handoff}");
    let task = fixture.runtime.get_task(&fixture.task_id).unwrap();
    assert_eq!(
        task.status,
        TaskStatus::Backlog,
        "held, neither rejected nor blocked"
    );
    let history = fixture.runtime.get_task_history(&fixture.task_id).unwrap();
    let latest = history
        .iter()
        .rev()
        .find(|entry| entry.to_status.is_some())
        .unwrap();
    assert_eq!(latest.event, BASELINE_RED_HOLD_EVENT);
    assert_eq!(
        latest.note.as_deref().and_then(BaselineRedHold::from_text),
        Some(hold.clone())
    );
    assert_eq!(git(&fixture.repo, &["rev-parse", "HEAD"]), candidate);
    // Assessed, so admission reaches the red-base check.
    fixture
        .runtime
        .update_task_with_identity(
            &fixture.task_id,
            TaskUpdateParams {
                complexity: Some(TaskComplexity::Low),
                ..Default::default()
            },
            Some("codex".into()),
            None,
        )
        .unwrap();
    assert_eq!(
        readiness(&fixture)["reason"],
        "baseline_red_hold",
        "admission withholds the task while the base is red"
    );

    // The executor records the handoff as the run's failure activity.
    let mut state = fixture
        .runtime
        .read_run_state(&run_id)
        .unwrap()
        .unwrap_or_else(|| {
            PipelineState::new(run_id.clone(), "task_pr_pipeline".into(), json!({}))
        });
    state.record_failure_activity(
        "pr_failure_handoff".into(),
        "review_gate_settle".into(),
        handoff,
    );
    fixture.runtime.write_run_state(&run_id, &state).unwrap();

    // The base turns green: the hold lifts, and the next delivery resumes
    // the kept candidate without implementing again.
    git(&fixture.repo, &["checkout", "--quiet", "main"]);
    std::fs::write(fixture.repo.join("check.sh"), GREEN).unwrap();
    git(&fixture.repo, &["commit", "--quiet", "-am", "fix the base"]);
    let green = git(&fixture.repo, &["rev-parse", "HEAD"]);
    let refresh = fixture.runtime.refresh_baseline_holds(None).unwrap();
    assert_eq!(refresh.lifted, vec![fixture.task_id.clone()], "{refresh:?}");
    // The fixture has no PR route, so admission still withholds it for that.
    assert_ne!(
        readiness(&fixture)["reason"],
        "baseline_red_hold",
        "a base where the check passes lifts the hold"
    );
    let previous = fixture.runtime.show_job_run(&run_id).unwrap();
    let next = fixture
        .runtime
        .insert_job_run("task_pr_pipeline", 1, Utc::now(), previous.input, None)
        .unwrap();
    fixture
        .runtime
        .update_task_with_identity(
            &fixture.task_id,
            TaskUpdateParams {
                status: Some(TaskStatus::InProgress),
                job_run_id: Some(Some(next.run_id.clone())),
                ..Default::default()
            },
            Some("codex".into()),
            None,
        )
        .unwrap();
    fixture.input["job_run_id"] = json!(next.run_id);
    git(&fixture.repo, &["checkout", "--quiet", "--detach", &green]);
    let resumed = execute_deterministic_action(
        &fixture.runtime,
        "candidate_resume",
        &json!({}),
        &json!({
            "job_run_id": next.run_id,
            "task_ids": [fixture.task_id],
            "workspace_path": fixture.input["workspace_path"],
            "base_sha": green,
            "prior_job_run_id": run_id,
        }),
        false,
        &HashMap::new(),
        None,
    )
    .unwrap();
    assert_eq!(resumed["outcome"], "resumed_validated", "{resumed}");
    assert_eq!(resumed["implement"], false, "no implementation step runs");
    assert_eq!(resumed["source_sha"], candidate.as_str());
    git(&fixture.repo, &["add", "-A"]);
    git(
        &fixture.repo,
        &["commit", "--quiet", "-m", "resumed candidate"],
    );

    // A fresh review judges it on the green base.
    fixture.admit();
    assert_ne!(
        fixture.input["admission"]["attempt_id"], settled.attempt_id,
        "the held attempt is not reused"
    );
    assert_eq!(fixture.input["admission"]["base_sha"], green.as_str());
    fixture.put_report(&json!({
        "schema_version": 1,
        "attempt_id": fixture.input["admission"]["attempt_id"],
        "verdict": "accept", "summary": "Checked on the green base.", "findings": [],
        "validation": [{"id": "V1", "command": CHECK, "outcome": "passed", "role": "required"}],
    }));
    let passed = fixture.settle().unwrap();
    assert_eq!(passed["gate"], "passed", "{passed}");
    assert!(certificate(&fixture).baseline_red.is_empty());
}

#[test]
fn a_failure_the_base_does_not_explain_is_never_held() {
    if !super::dispatch_admission::isolated(
        "review_baseline_hold::a_failure_the_base_does_not_explain_is_never_held",
    ) {
        return;
    }
    // The candidate fails beyond the base: its own failure stands.
    let mut superset = fixture(RED, "after, regressed\n");
    let failure = settle_claim(&mut superset, CHECK, &["check.sh"]);
    assert!(!is_baseline_red_failure(None, Some(&failure)), "{failure}");
    let settled = certificate(&superset);
    assert_eq!(settled.verdict, ReviewVerdict::Reject);
    assert!(settled.baseline_red.is_empty());
    assert!(
        settled
            .escalation
            .as_deref()
            .is_some_and(|reason| reason.contains("baseline_exceeded")
                && reason.contains("suite::regressed")),
        "{:?}",
        settled.escalation
    );

    // The base passes: host verification contradicts the claim.
    let mut contradicted = fixture(GREEN_ON_BASE, "after\n");
    let failure = settle_claim(&mut contradicted, CHECK, &["check.sh"]);
    assert!(!is_baseline_red_failure(None, Some(&failure)), "{failure}");
    let settled = certificate(&contradicted);
    assert_eq!(settled.verdict, ReviewVerdict::Incomplete);
    assert!(settled.baseline_red.is_empty());
    assert!(
        settled
            .escalation
            .as_deref()
            .is_some_and(|reason| reason.contains("baseline_claim_refused")),
        "{:?}",
        settled.escalation
    );

    // A failure inside the candidate's scope is its own, whatever the base.
    let mut in_scope = fixture(RED, "after\n");
    settle_claim(&mut in_scope, CHECK, &["candidate.txt"]);
    let settled = certificate(&in_scope);
    assert_eq!(settled.verdict, ReviewVerdict::Incomplete);
    assert!(settled.baseline_red.is_empty());

    // A command the host does not trust is refused without running it on
    // the host.
    let mut untrusted = fixture(RED, "after\n");
    settle_claim(&mut untrusted, "sh untrusted.sh", &["untrusted.sh"]);
    let settled = certificate(&untrusted);
    assert_eq!(settled.verdict, ReviewVerdict::Incomplete);
    assert!(settled.baseline_red.is_empty());
    assert!(
        !untrusted.repo.join(".orbit/tmp/untrusted-ran").exists(),
        "settlement never runs reviewer-authored command text"
    );
    for fixture in [&superset, &contradicted, &in_scope, &untrusted] {
        assert_eq!(
            fixture.runtime.get_task(&fixture.task_id).unwrap().status,
            TaskStatus::InProgress,
            "no hold is recorded"
        );
    }
}

/// A task held for [`CHECK`] failing on `main`, recorded by the real failure
/// handoff, and assessed so admission reaches the red-base check.
pub(super) fn held_on_red_base() -> (Fixture, BaselineRedHold) {
    let fixture = fixture(RED, "after\n");
    let run_id = fixture.input["job_run_id"].as_str().unwrap().to_string();
    let hold = BaselineRedHold {
        base_ref: "main".into(),
        base_sha: git(&fixture.repo, &["rev-parse", "main"]),
        command: CHECK.into(),
        run_id: run_id.clone(),
    };
    let handoff = execute_deterministic_action(
        &fixture.runtime,
        "pr_failure_handoff",
        &json!({}),
        &json!({
            "failed_step_id": "validate",
            "error_code": "deterministic_action_refused",
            "error_message": hold.text("required validation 'sh check.sh' fails on the base too"),
            "run_id": run_id,
            "job_input": {"task_ids": [fixture.task_id]},
            "pipeline": {"worktree": {
                "job_run_id": run_id, "workspace_path": fixture.input["workspace_path"],
            }},
        }),
        false,
        &HashMap::new(),
        None,
    )
    .unwrap();
    assert_eq!(handoff["decision"], "held_baseline_red", "{handoff}");
    fixture
        .runtime
        .update_task_with_identity(
            &fixture.task_id,
            TaskUpdateParams {
                complexity: Some(TaskComplexity::Low),
                ..Default::default()
            },
            Some("codex".into()),
            None,
        )
        .unwrap();
    (fixture, hold)
}

/// [ORB-14739] A held task whose base tip moved to a commit where the check
/// would run for minutes does not block the readiness explanation or the
/// drain's backlog snapshot: both read the hold's recorded verdict and never
/// run the command. Only a refresh run the owner's clock dispatches does.
#[test]
fn a_moved_base_never_runs_the_check_inside_a_snapshot() {
    if !super::dispatch_admission::isolated(
        "review_baseline_hold::a_moved_base_never_runs_the_check_inside_a_snapshot",
    ) {
        return;
    }
    let (fixture, hold) = held_on_red_base();

    // The base moves to a check that marks it ran, then outlasts the test.
    let marker = fixture.repo.parent().unwrap().join("base-check-ran");
    git(&fixture.repo, &["checkout", "--quiet", "main"]);
    std::fs::write(
        fixture.repo.join("check.sh"),
        format!(
            "#!/bin/sh\ntouch '{}'\nsleep 120\nexit 0\n",
            marker.display()
        ),
    )
    .unwrap();
    git(
        &fixture.repo,
        &["commit", "--quiet", "-am", "slow base check"],
    );
    git(&fixture.repo, &["checkout", "--quiet", "candidate"]);

    let started = std::time::Instant::now();
    let entry = readiness(&fixture);
    let backlog = fixture
        .runtime
        .run_deterministic(
            "list_backlog_tasks",
            &json!({}),
            &json!({}),
            Default::default(),
        )
        .unwrap();
    let elapsed = started.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(60),
        "the snapshots waited {elapsed:?} on the base check"
    );
    assert!(!marker.exists(), "a snapshot ran the required command");
    assert_eq!(entry["reason"], "baseline_red_hold", "{entry}");
    assert!(
        entry["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains(&hold.base_sha)),
        "with no verdict yet the recorded hold stands: {entry}"
    );
    assert!(
        backlog["excluded"].as_array().is_some_and(|excluded| {
            excluded.iter().any(|row| {
                row["id"] == fixture.task_id.as_str() && row["reason"] == "baseline_red_hold"
            })
        }),
        "{backlog}"
    );

    // A tick whose budget is already spent reads no hold, so it neither
    // checks one nor dispatches a refresh run.
    let refresh = fixture
        .runtime
        .run_baseline_hold_tick(std::time::Instant::now())
        .unwrap();
    assert_eq!(
        refresh,
        orbit_core::application::task::BaselineHoldRefresh::default()
    );
    assert!(!marker.exists(), "an expired tick ran the required command");
}

/// An accepting report that files a failed `command` as a diagnostic, its
/// sources outside the candidate's scope.
fn diagnostic_report(fixture: &Fixture, command: &str, sources: &[&str]) -> Value {
    json!({
        "schema_version": 1,
        "attempt_id": fixture.input["admission"]["attempt_id"],
        "verdict": "accept",
        "summary": "The candidate is sound; a wider check failed elsewhere.",
        "findings": [],
        "validation": [
            {"id": "V1", "command": "fixture check", "outcome": "passed", "role": "required"},
            {
                "command": command, "outcome": "failed", "role": "diagnostic",
                "note": "Fails only in files the candidate does not touch.",
                "sources": sources,
            },
        ],
    })
}

/// [ORB-14684] The incident shape through the real settlement: a check the
/// owner lists in `review.baseline_commands`, filed as a failed diagnostic on
/// the reviewer's own out-of-scope sources, never settles as a pass. It
/// needs a passing record or a baseline claim settlement reproduces, and the
/// certificate records the trusted list its consumers judge it by. An
/// unlisted command's failure stays an honest diagnostic.
#[test]
fn a_listed_check_filed_as_a_failed_diagnostic_settles_incomplete() {
    if !super::dispatch_admission::isolated(
        "review_baseline_hold::a_listed_check_filed_as_a_failed_diagnostic_settles_incomplete",
    ) {
        return;
    }
    let mut listed = fixture(RED, "after\n");
    listed.admit();
    listed.put_report(&diagnostic_report(&listed, CHECK, &["check.sh"]));
    let failure = listed
        .settle()
        .expect_err("a trusted gate's failure is never a pass")
        .to_string();
    assert!(!is_baseline_red_failure(None, Some(&failure)), "{failure}");
    let settled = certificate(&listed);
    assert_eq!(settled.verdict, ReviewVerdict::Incomplete);
    assert!(!settled.validation_complete);
    assert_eq!(settled.baseline_commands, vec![CHECK.to_string()]);
    let escalation = settled.escalation.unwrap_or_default();
    assert!(
        escalation.contains(&format!("`{CHECK}`"))
            && escalation.contains("cannot be recorded as diagnostic")
            && escalation.contains("passing record or a baseline claim"),
        "the reason names the command and what a trusted gate needs: {escalation}"
    );

    let mut unlisted = fixture(RED, "after\n");
    unlisted.admit();
    unlisted.put_report(&diagnostic_report(
        &unlisted,
        "sh untrusted.sh",
        &["untrusted.sh"],
    ));
    let passed = unlisted.settle().unwrap();
    assert_eq!(passed["gate"], "passed", "{passed}");
    assert!(certificate(&unlisted).validation_complete);
}

/// [ORB-14684] A baseline claim naming a base other than the one the review
/// pinned is refused before anything runs, and the review settles
/// `incomplete`.
#[test]
fn a_baseline_claim_on_another_base_is_refused() {
    if !super::dispatch_admission::isolated(
        "review_baseline_hold::a_baseline_claim_on_another_base_is_refused",
    ) {
        return;
    }
    let mut fixture = fixture(RED, "after\n");
    fixture.admit();
    let mut report = claim_report(&fixture, CHECK, &["check.sh"]);
    report["validation"][0]["baseline"]["base_commit"] =
        json!("0000000000000000000000000000000000000000");
    fixture.put_report(&report);
    let failure = fixture
        .settle()
        .expect_err("a failed required check never passes")
        .to_string();
    assert!(!is_baseline_red_failure(None, Some(&failure)), "{failure}");
    let settled = certificate(&fixture);
    assert_eq!(settled.verdict, ReviewVerdict::Incomplete);
    assert!(settled.baseline_red.is_empty());
    assert!(
        settled.escalation.as_deref().is_some_and(|reason| {
            reason.contains("baseline_claim_refused")
                && reason.contains("but the review pinned base")
        }),
        "{:?}",
        settled.escalation
    );
}
