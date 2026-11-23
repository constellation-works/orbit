//! An incomplete before-landing review left its reviewer's commit on the
//! published head; the failure handoff pushed it and kept the PR, and the run
//! ended held for a review decision. An operator requeue resumes that exact
//! PR at the attested reviewer head, so the next run validates and reviews it
//! afresh. A moved remote or local head, a receipt that does not attest the
//! push, a changed specification or a live owner stays refused with nothing
//! written, and a repeated requeue starts nothing new.
use super::escalated_publication::green_forge;
use super::failed_publication::escalate;
use super::handoff_requeue::{
    assert_next_run_resumes_the_published_candidate, refused, requeue, status,
};
use super::published_owner::save_run;
use super::*;
use std::path::Path;

const SETTLE: &str = "landing_review_gate_settle";
const VALIDATE: &str = "landing_review_validate";
const LANDING_PUSH: &str = "landing_push";

/// A receipt case: its name and how it rewrites the handoff receipt.
type ReceiptCase<'a> = (&'a str, &'a dyn Fn(&mut Value));

/// The incident's run: published at P, then the before-landing review
/// committed a correction R and settled `incomplete`; the real failure
/// handoff pushed R onto P and kept PR 17. Returns the fixture with
/// `f.head` = R, the checkout and P.
fn held_reviewer_push_fixture() -> (Fixture, PathBuf, String) {
    let mut f = Fixture::new();
    let path = f
        ._root
        .path()
        .join("global/resources/jobs/task_pr_pipeline.yaml");
    let yaml = std::fs::read_to_string(&path).unwrap();
    std::fs::write(
        &path,
        yaml.replace(
            "  task_delivery: {}",
            "  task_delivery: {}\n  failure_activity: pr_failure_handoff",
        ) + &format!("    - {{id: {SETTLE}, target: 'activity:review_gate_settle'}}\n"),
    )
    .unwrap();
    let checkout = published_owner::publication(&mut f);
    escalate(&f, &f.source.run_id);
    let published = f.head.clone();
    std::fs::write(checkout.join("review.txt"), "reviewer correction\n").unwrap();
    git(&checkout, &["add", "review.txt"]);
    git(
        &checkout,
        &["commit", "-qm", "review: correct the HTTP test"],
    );
    let reviewed = git(&checkout, &["rev-parse", "HEAD"]);
    let output = f.action(
        "pr_failure_handoff",
        json!({"run_id":f.source.run_id,
        "failed_step_id":SETTLE,"activity_name":"review_gate_settle","error_code":"execution",
        "error_message":"review_gate_blocked: verdict incomplete (missing timing evidence)",
        "job_input":f.source.input,"pipeline":f.state.pipeline}),
    );
    assert_eq!(output["decision"], "landing_review_failure", "{output}");
    assert_eq!(output["reviewer_push"]["local_sha"], reviewed, "{output}");
    assert_eq!(output["reviewer_push"]["published_head_sha"], published);
    let remote = git(&f.repo, &["ls-remote", "origin", &f.branch]);
    assert!(
        remote.starts_with(&reviewed),
        "the handoff pushed R: {remote}"
    );
    let mut answer: Value = serde_json::from_slice(&std::fs::read(&f.answer).unwrap()).unwrap();
    answer["headRefOid"] = json!(reviewed);
    std::fs::write(&f.answer, answer.to_string()).unwrap();
    let mut state = f.runtime.read_run_state(&f.source.run_id).unwrap().unwrap();
    state.record_failure_activity("pr_failure_handoff".into(), SETTLE.into(), output);
    f.runtime.write_run_state(&f.source.run_id, &state).unwrap();
    f.state = state;
    let mut run = f.runtime.show_job_run_observed(&f.source.run_id).unwrap();
    run.state = JobRunState::Held;
    run.pid = None;
    run.pid_start_time = None;
    run.finished_at = Some(Utc::now());
    save_run(&f, &run);
    assert!(
        f.runtime
            .complete_job_run_step(
                &run.run_id,
                &orbit_store::contracts::JobRunStepParams {
                    step_index: 40,
                    target_type: orbit_types::workflow::JobTargetType::Activity,
                    target_id: SETTLE.into(),
                    state: JobRunState::Held,
                    started_at: Utc::now(),
                    finished_at: Utc::now(),
                    duration_ms: Some(1),
                    exit_code: Some(0),
                    lock_wait_ms: None,
                    agent_response_json: None,
                    error_code: Some(
                        orbit_types::workflow::REVIEW_DECISION_PENDING_ERROR_CODE.into(),
                    ),
                    error_message: Some(format!(
                        "before-landing review did not approve; {}",
                        orbit_types::workflow::REVIEW_LANDING_DECISION_PENDING
                    )),
                },
            )
            .unwrap()
    );
    f.source = f.runtime.show_job_run_observed(&run.run_id).unwrap();
    f.head = reviewed;
    green_forge(&mut f);
    (f, checkout, published)
}

fn rewrite_receipt(f: &Fixture, edit: impl FnOnce(&mut Value)) -> Value {
    let mut state = f.runtime.read_run_state(&f.source.run_id).unwrap().unwrap();
    let original = serde_json::to_value(&state).unwrap();
    edit(&mut state.failure_activity_checkpoint.as_mut().unwrap().output);
    f.runtime.write_run_state(&f.source.run_id, &state).unwrap();
    original
}

fn restore_receipt(f: &Fixture, original: Value) {
    f.runtime
        .write_run_state(&f.source.run_id, &serde_json::from_value(original).unwrap())
        .unwrap();
}

/// Move `branch` in `repo` to `sha` (local ref or, through a push, the remote).
fn force_branch(repo: &Path, branch: &str, sha: &str, remote: bool) {
    if remote {
        git(
            repo,
            &[
                "push",
                "-qf",
                "origin",
                &format!("{sha}:refs/heads/{branch}"),
            ],
        );
    } else {
        git(repo, &["update-ref", &format!("refs/heads/{branch}"), sha]);
    }
}

/// The incident: the requeue refused 'retained local branch moved' because it
/// bound the PR to P while the attested reviewer push had carried it to R.
fn a_held_incomplete_review_requeue_resumes_the_attested_reviewer_head() {
    let (f, _, published) = held_reviewer_push_fixture();
    assert_eq!(status(&f), TaskStatus::Blocked);
    let answer = requeue(&f, false).unwrap();
    assert_eq!(answer["status"], "backlog", "{answer}");
    assert_eq!(answer["resumes"]["pull_request"], "17");
    assert_eq!(answer["resumes"]["head_sha"], f.head);
    assert_ne!(f.head, published);
    // A repeated requeue starts nothing and writes no second binding.
    assert!(requeue(&f, false).is_err());
    let preserved = f
        .runtime
        .get_task_history(&f.task)
        .unwrap()
        .into_iter()
        .filter(|entry| entry.event == "published_owner_requeued")
        .count();
    assert_eq!(preserved, 1);
    assert_next_run_resumes_the_published_candidate(&f);
}

/// Every unproven neighbour stays refused with nothing written, and the
/// original then still requeues, so each refusal was its own case's.
fn unattested_or_moved_reviewer_heads_stay_refused() {
    let (f, checkout, published) = held_reviewer_push_fixture();
    let head = f.head.clone();
    let branch = f.branch.clone();
    // Changed local head: the retained branch is no longer the attested R.
    force_branch(&f.repo, &branch, &published, false);
    assert!(refused(&f, false, "local head").contains("moved"));
    force_branch(&f.repo, &branch, &head, false);
    // Changed remote head.
    force_branch(&checkout, &branch, &published, true);
    assert!(refused(&f, false, "remote head").contains("drifted"));
    force_branch(&checkout, &branch, &head, true);
    // Missing or conflicting attestation.
    let cases: [ReceiptCase; 4] = [
        ("missing", &|o| o["reviewer_push"] = Value::Null),
        ("other lease", &|o| {
            o["reviewer_push"]["published_head_sha"] = json!(head)
        }),
        ("other branch", &|o| {
            o["reviewer_push"]["branch"] = json!("elsewhere")
        }),
        ("rewrite", &|o| {
            o["reviewer_push"]["decision"] = json!("performed_force_with_lease")
        }),
    ];
    for (case, edit) in cases {
        let original = rewrite_receipt(&f, edit);
        let error = refused(&f, false, case);
        assert!(
            error.contains("moved") || error.contains("attestation disagrees"),
            "{case}: {error}"
        );
        restore_receipt(&f, original);
    }
    // An active owner.
    let stopped = f.runtime.show_job_run_observed(&f.source.run_id).unwrap();
    let mut live = stopped.clone();
    live.state = JobRunState::Running;
    live.pid = Some(std::process::id());
    live.finished_at = None;
    save_run(&f, &live);
    refused(&f, false, "live owner");
    save_run(&f, &stopped);
    let answer = requeue(&f, false).unwrap();
    assert_eq!(answer["resumes"]["head_sha"], head, "{answer}");
}

/// Criteria edited after the review are refused before anything moves.
fn a_changed_specification_refuses_the_reviewer_head() {
    let (f, _, _) = held_reviewer_push_fixture();
    f.runtime
        .update_task_as_human(
            &f.task,
            orbit_core::application::task::TaskUpdateParams {
                acceptance_criteria: Some(vec!["A different guarantee".into()]),
                ..Default::default()
            },
            "human".into(),
        )
        .unwrap();
    let error = refused(&f, false, "changed specification");
    assert!(error.contains("specification changed"), "{error}");
}

/// A rejected before-landing review left its fix commit R on `published` P and
/// `pr_failure_handoff` preserved R onto `refs/orbit/candidates/<task>/<run>`
/// without pushing to the PR branch. Requeue keeps PR head at P while adopting
/// R as `candidate_head_sha` so the resumed run starts from R's repair.
fn a_rejected_landing_review_requeue_resumes_the_preserved_fix_commit() {
    let mut f = Fixture::new();
    let path = f
        ._root
        .path()
        .join("global/resources/jobs/task_pr_pipeline.yaml");
    let yaml = std::fs::read_to_string(&path).unwrap();
    std::fs::write(
        &path,
        yaml.replace(
            "  task_delivery: {}",
            "  task_delivery: {}\n  failure_activity: pr_failure_handoff",
        ) + &format!("    - {{id: {SETTLE}, target: 'activity:review_gate_settle'}}\n"),
    )
    .unwrap();
    let checkout = published_owner::publication(&mut f);
    escalate(&f, &f.source.run_id);
    let published = f.head.clone();
    std::fs::write(checkout.join("review.txt"), "rejected review repair\n").unwrap();
    git(&checkout, &["add", "review.txt"]);
    git(
        &checkout,
        &["commit", "-qm", "review: fix rules before reject"],
    );
    let reviewed = git(&checkout, &["rev-parse", "HEAD"]);
    let expected_ref = format!("refs/orbit/candidates/{}/{}", f.task, f.source.run_id);
    let output = f.action(
        "pr_failure_handoff",
        json!({"run_id":f.source.run_id,
        "failed_step_id":SETTLE,"activity_name":"review_gate_settle","error_code":"review_gate_blocked",
        "error_message":"review_gate_blocked: verdict reject (needs another check)",
        "job_input":f.source.input,"pipeline":f.state.pipeline}),
    );
    assert_eq!(output["decision"], "landing_review_failure", "{output}");
    assert_eq!(output["reviewer_push"], Value::Null, "{output}");
    assert_eq!(
        output["reviewer_preservation"]["decision"], "preserved",
        "{output}"
    );
    assert_eq!(
        output["reviewer_preservation"]["commit_sha"], reviewed,
        "{output}"
    );
    assert_eq!(
        output["reviewer_preservation"]["ref"], expected_ref,
        "{output}"
    );
    assert!(
        git(&f.repo, &["ls-remote", "origin", &f.branch]).starts_with(&published),
        "reject never pushes to the PR head branch"
    );
    assert!(
        git(&f.repo, &["ls-remote", "origin", &expected_ref]).starts_with(&reviewed),
        "reject fix commit is reachable on durable ref"
    );

    let mut answer: Value = serde_json::from_slice(&std::fs::read(&f.answer).unwrap()).unwrap();
    answer["headRefOid"] = json!(published);
    std::fs::write(&f.answer, answer.to_string()).unwrap();

    let mut state = f.runtime.read_run_state(&f.source.run_id).unwrap().unwrap();
    state.record_failure_activity("pr_failure_handoff".into(), SETTLE.into(), output.clone());
    f.runtime.write_run_state(&f.source.run_id, &state).unwrap();
    f.state = state;
    let mut run = f.runtime.show_job_run_observed(&f.source.run_id).unwrap();
    run.state = JobRunState::Held;
    run.pid = None;
    run.pid_start_time = None;
    run.finished_at = Some(Utc::now());
    save_run(&f, &run);
    assert!(
        f.runtime
            .complete_job_run_step(
                &run.run_id,
                &orbit_store::contracts::JobRunStepParams {
                    step_index: 40,
                    target_type: orbit_types::workflow::JobTargetType::Activity,
                    target_id: SETTLE.into(),
                    state: JobRunState::Held,
                    started_at: Utc::now(),
                    finished_at: Utc::now(),
                    duration_ms: Some(1),
                    exit_code: Some(0),
                    lock_wait_ms: None,
                    agent_response_json: None,
                    error_code: Some(
                        orbit_types::workflow::REVIEW_DECISION_PENDING_ERROR_CODE.into(),
                    ),
                    error_message: Some(format!(
                        "before-landing review did not approve; {}",
                        orbit_types::workflow::REVIEW_LANDING_DECISION_PENDING
                    )),
                },
            )
            .unwrap()
    );
    f.source = f.runtime.show_job_run_observed(&run.run_id).unwrap();
    green_forge(&mut f);

    // Tampered preservation attestation is refused rather than trusted.
    let saved = rewrite_receipt(&f, |receipt| {
        receipt["reviewer_preservation"]["published_head_sha"] = json!(reviewed);
    });
    refused(&f, false, "mismatched published_head_sha");
    restore_receipt(&f, saved);

    let answer = requeue(&f, false).unwrap();
    assert_eq!(answer["status"], "backlog", "{answer}");
    assert_eq!(answer["resumes"]["pull_request"], "17");
    assert_eq!(answer["resumes"]["head_sha"], published);
    assert_eq!(answer["resumes"]["candidate_head_sha"], reviewed);
    assert_eq!(answer["resumes"]["candidate_ref"], expected_ref);

    let (run, setup) = super::published_owner::next(&f);
    let next_checkout = PathBuf::from(setup["workspace_path"].as_str().unwrap());
    let resumed = f.action(
        "candidate_resume",
        json!({"job_run_id":run.run_id,"task_ids":[f.task],
        "workspace_path":next_checkout,"base_sha":setup["base_sha"]}),
    );
    assert_eq!(resumed["outcome"], "resumed_carried", "{resumed}");
    assert_eq!(resumed["reused_head_sha"], published);
    assert_eq!(resumed["source_sha"], reviewed);
    assert_eq!(
        std::fs::read_to_string(next_checkout.join("review.txt")).unwrap(),
        "rejected review repair\n"
    );
}

/// Completed and validated before-landing review whose `landing_push` failed
/// while keeping PR 17 and `origin/<branch>` at `published` P, with
/// `pr_failure_handoff` preserving R on `refs/orbit/candidates/<task>/<run>`.
fn failed_landing_push_fixture() -> (Fixture, PathBuf, String, String) {
    let mut f = Fixture::new();
    let path = f
        ._root
        .path()
        .join("global/resources/jobs/task_pr_pipeline.yaml");
    let yaml = std::fs::read_to_string(&path).unwrap();
    std::fs::write(
        &path,
        yaml.replace(
            "  task_delivery: {}",
            "  task_delivery: {}\n  failure_activity: pr_failure_handoff",
        ) + &format!(
            "    - {{id: {SETTLE}, target: 'activity:review_gate_settle'}}\n\
             \x20   - {{id: {VALIDATE}, target: 'activity:candidate_validate', default_input: {{ownership_base_sha: \"{{{{ steps.{SETTLE}.output.implementation_head_sha }}}}\"}}}}\n\
             \x20   - {{id: {LANDING_PUSH}, target: 'activity:git_push', default_input: {{lease_remote_sha: \"{{{{ steps.push.output.local_sha }}}}\", pr_number: \"{{{{ steps.pr_open.output.pr_number }}}}\"}}}}\n"
        ),
    )
    .unwrap();
    let checkout = published_owner::publication(&mut f);
    let published = f.head.clone();
    std::fs::write(checkout.join("review.txt"), "landing push repair\n").unwrap();
    git(&checkout, &["add", "review.txt"]);
    git(
        &checkout,
        &["commit", "-qm", "review: validated fix before landing_push"],
    );
    let reviewed = git(&checkout, &["rev-parse", "HEAD"]);

    let settle_output = json!({
        "gate": "passed",
        "reviewer_fixed": true,
        "implementation_head_sha": published,
        "reviewed_head_sha": reviewed,
        "reviewed_base_sha": f.base,
    });
    let validate_output = json!({
        "phase": "validate",
        "decision": "passed",
        "tested_head": reviewed,
        "branch": f.branch,
    });
    published_owner::checkpoint(&f, &f.source.run_id, 8, SETTLE, settle_output);
    published_owner::checkpoint(&f, &f.source.run_id, 9, VALIDATE, validate_output);
    f.state = f.runtime.read_run_state(&f.source.run_id).unwrap().unwrap();

    let expected_ref = format!("refs/orbit/candidates/{}/{}", f.task, f.source.run_id);
    let handoff = f.action(
        "pr_failure_handoff",
        json!({
            "run_id": f.source.run_id,
            "failed_step_id": LANDING_PUSH,
            "activity_name": "git_push",
            "error_code": "execution",
            "error_message": "git push failed: remote rejected temporary hook",
            "job_input": f.source.input,
            "pipeline": f.state.pipeline,
        }),
    );
    assert_eq!(handoff["decision"], "landing_review_failure", "{handoff}");
    assert_eq!(handoff["failed_step_id"], LANDING_PUSH, "{handoff}");
    assert_eq!(handoff["reviewer_push"], Value::Null, "{handoff}");
    assert_eq!(
        handoff["reviewer_preservation"]["decision"], "preserved",
        "{handoff}"
    );
    assert_eq!(
        handoff["reviewer_preservation"]["commit_sha"], reviewed,
        "{handoff}"
    );
    assert_eq!(
        handoff["reviewer_preservation"]["published_head_sha"], published,
        "{handoff}"
    );
    assert_eq!(
        handoff["reviewer_preservation"]["ref"], expected_ref,
        "{handoff}"
    );
    assert!(
        git(&f.repo, &["ls-remote", "origin", &f.branch]).starts_with(&published),
        "failed landing_push leaves origin PR branch at published head P"
    );
    assert!(
        git(&f.repo, &["ls-remote", "origin", &expected_ref]).starts_with(&reviewed),
        "failed landing_push fix commit R is reachable on durable ref"
    );

    escalate(&f, &f.source.run_id);
    let mut state = f.runtime.read_run_state(&f.source.run_id).unwrap().unwrap();
    state.record_failure_activity("pr_failure_handoff".into(), LANDING_PUSH.into(), handoff);
    f.runtime.write_run_state(&f.source.run_id, &state).unwrap();
    f.state = state;

    let mut run = f.runtime.show_job_run_observed(&f.source.run_id).unwrap();
    run.state = JobRunState::Failed;
    run.pid = None;
    run.pid_start_time = None;
    run.finished_at = Some(Utc::now());
    save_run(&f, &run);
    f.source = f.runtime.show_job_run_observed(&run.run_id).unwrap();
    green_forge(&mut f);
    (f, checkout, published, reviewed)
}

/// A failed `landing_push` whose PR and origin branch remained at `published` P
/// admits its preserved reviewer repair R through explicit `source_repair`
/// (`inspect` + `submit`) and applies R onto the successor checkout, including
/// when the local branch in `repo` stayed at P.
fn a_failed_landing_push_source_repair_admits_and_applies_preserved_fix() {
    let (f, checkout, published, reviewed) = failed_landing_push_fixture();
    let expected_ref = format!("refs/orbit/candidates/{}/{}", f.task, f.source.run_id);
    // Simulate a run whose worktree detached or left `refs/heads/{branch}` at P
    // while R lives on the durable candidate ref.
    git(&checkout, &["checkout", "--quiet", "--detach"]);
    force_branch(&f.repo, &f.branch, &published, false);

    let inspection = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", f.request("inspect"))
        .unwrap();
    assert_eq!(inspection["binding"]["head_sha"], published);
    assert_eq!(inspection["binding"]["candidate_head_sha"], reviewed);
    assert_eq!(inspection["binding"]["candidate_ref"], expected_ref);

    let mut submit = f.submit(inspection["binding"].clone());
    submit["source_repair"]["policy_digest"] = inspection["policy_digest"].clone();
    submit["source_repair"]["unmet_acceptance"] = json!(
        "landing_push failed after validated before-landing review; carry the preserved repair."
    );
    let submitted = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", submit)
        .unwrap();
    let id = submitted["run_id"].as_str().unwrap();
    f.wait(id);
    f.start_actions(id);

    let setup = f.action(
        "worktree_setup",
        json!({"job_run_id":id,"task_ids":[f.task],"base":"main","base_sync":"remote"}),
    );
    let next_checkout = PathBuf::from(setup["workspace_path"].as_str().unwrap());
    let resumed = f.action(
        "candidate_resume",
        json!({"job_run_id":id,"task_ids":[f.task],"workspace_path":next_checkout,"base_sha":setup["base_sha"]}),
    );
    assert_eq!(resumed["outcome"], "resumed_repaired", "{resumed}");
    assert_eq!(resumed["reused_head_sha"], published);
    assert_eq!(resumed["source_sha"], reviewed);
    assert_eq!(resumed["reused_branch"], f.branch);
    assert_eq!(
        std::fs::read_to_string(next_checkout.join("review.txt")).unwrap(),
        "landing push repair\n"
    );
}

/// An operator requeue after a failed `landing_push` also resumes PR 17 at P
/// while carrying the preserved reviewer fix commit R onto the successor checkout.
fn a_failed_landing_push_requeue_resumes_the_preserved_fix_commit() {
    let (f, _, published, reviewed) = failed_landing_push_fixture();
    let expected_ref = format!("refs/orbit/candidates/{}/{}", f.task, f.source.run_id);

    let answer = requeue(&f, false).unwrap();
    assert_eq!(answer["status"], "backlog", "{answer}");
    assert_eq!(answer["resumes"]["pull_request"], "17");
    assert_eq!(answer["resumes"]["head_sha"], published);
    assert_eq!(answer["resumes"]["candidate_head_sha"], reviewed);
    assert_eq!(answer["resumes"]["candidate_ref"], expected_ref);

    let (run, setup) = super::published_owner::next(&f);
    let next_checkout = PathBuf::from(setup["workspace_path"].as_str().unwrap());
    let resumed = f.action(
        "candidate_resume",
        json!({"job_run_id":run.run_id,"task_ids":[f.task],
        "workspace_path":next_checkout,"base_sha":setup["base_sha"]}),
    );
    assert_eq!(resumed["outcome"], "resumed_carried", "{resumed}");
    assert_eq!(resumed["reused_head_sha"], published);
    assert_eq!(resumed["source_sha"], reviewed);
    assert_eq!(
        std::fs::read_to_string(next_checkout.join("review.txt")).unwrap(),
        "landing push repair\n"
    );
}

/// A completed `landing_push`, changed PR/origin/local lease, or mismatched
/// preservation receipt stays refused with nothing written.
fn failed_landing_push_keeps_completed_lease_and_receipt_refusals() {
    let (f, checkout, published, reviewed) = failed_landing_push_fixture();
    git(&checkout, &["checkout", "--quiet", "--detach"]);
    let branch = f.branch.clone();
    let clean_state =
        serde_json::to_value(f.runtime.read_run_state(&f.source.run_id).unwrap()).unwrap();
    let clean_answer = std::fs::read_to_string(&f.answer).unwrap();
    let restore = |f: &Fixture| {
        f.runtime
            .write_run_state(
                &f.source.run_id,
                &serde_json::from_value(clean_state.clone()).unwrap(),
            )
            .unwrap();
        std::fs::write(&f.answer, &clean_answer).unwrap();
        force_branch(&f.repo, &branch, &reviewed, false);
        force_branch(&checkout, &branch, &published, true);
    };
    let assert_both_refuse = |f: &Fixture, case: &str| {
        let requeue_err = refused(f, false, case);
        let repair_err = f
            .runtime
            .run_tool_as_human("orbit.workflow.run.resume", f.request("inspect"))
            .expect_err(&format!("{case}: inspect unexpectedly succeeded"))
            .to_string();
        assert!(
            requeue_err.contains("source repair") || requeue_err.contains("Nothing was changed"),
            "{case}: requeue={requeue_err}"
        );
        assert!(
            repair_err.contains("source repair refused"),
            "{case}: repair={repair_err}"
        );
    };

    // 1. Completed landing_push (output present and step succeeded, or either alone).
    for mode in ["both", "output_only", "step_only"] {
        let mut state = f.runtime.read_run_state(&f.source.run_id).unwrap().unwrap();
        let output = json!({
            "phase": "push",
            "branch": branch,
            "local_sha": reviewed,
            "remote_sha_before": published,
        });
        if mode != "step_only" {
            state.record_pipeline_output(LANDING_PUSH, output.clone());
        }
        if mode != "output_only" {
            state.record_step(10, JobRunState::Success, Some(output), None);
        }
        f.runtime.write_run_state(&f.source.run_id, &state).unwrap();
        assert_both_refuse(&f, &format!("completed_landing_push_{mode}"));
        restore(&f);
    }

    // 2. Changed lease: origin moved, PR head moved, or local branch diverged.
    force_branch(&checkout, &branch, &reviewed, true);
    assert_both_refuse(&f, "origin_lease_moved");
    restore(&f);

    let mut answer: Value = serde_json::from_str(&clean_answer).unwrap();
    answer["headRefOid"] = json!(reviewed);
    std::fs::write(&f.answer, answer.to_string()).unwrap();
    assert_both_refuse(&f, "pr_head_moved");
    restore(&f);

    force_branch(&f.repo, &branch, &f.base, false);
    assert_both_refuse(&f, "local_branch_diverged");
    restore(&f);

    // 3. Mismatched preservation receipt (including a different descendant commit).
    std::fs::write(checkout.join("other.txt"), "other commit\n").unwrap();
    git(&checkout, &["add", "other.txt"]);
    git(&checkout, &["commit", "-qm", "other descendant commit"]);
    let other_descendant = git(&checkout, &["rev-parse", "HEAD"]);
    let cases: [ReceiptCase; 5] = [
        ("mismatched_published_head", &|o| {
            o["reviewer_preservation"]["published_head_sha"] = json!(reviewed);
        }),
        ("mismatched_commit_vs_settled", &|o| {
            o["reviewer_preservation"]["commit_sha"] = json!(other_descendant);
        }),
        ("mismatched_ref", &|o| {
            o["reviewer_preservation"]["ref"] = json!("refs/orbit/candidates/other/run");
        }),
        ("mismatched_run", &|o| {
            o["handoff_run_id"] = json!("jrun-other");
        }),
        ("mismatched_decision", &|o| {
            o["decision"] = json!("blocked_failure_pr");
        }),
    ];
    for (case, edit) in cases {
        let original = rewrite_receipt(&f, edit);
        assert_both_refuse(&f, case);
        restore_receipt(&f, original);
    }

    // Confirm the uncorrupted fixture still admits after all refusal checks.
    let inspection = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", f.request("inspect"))
        .unwrap();
    assert_eq!(inspection["binding"]["head_sha"], published);
    assert_eq!(inspection["binding"]["candidate_head_sha"], reviewed);
}

crate::scenario_table!(
    reviewer_push_scenarios,
    a_held_incomplete_review_requeue_resumes_the_attested_reviewer_head,
    unattested_or_moved_reviewer_heads_stay_refused,
    a_changed_specification_refuses_the_reviewer_head,
    a_rejected_landing_review_requeue_resumes_the_preserved_fix_commit,
    a_failed_landing_push_source_repair_admits_and_applies_preserved_fix,
    a_failed_landing_push_requeue_resumes_the_preserved_fix_commit,
    failed_landing_push_keeps_completed_lease_and_receipt_refusals
);
