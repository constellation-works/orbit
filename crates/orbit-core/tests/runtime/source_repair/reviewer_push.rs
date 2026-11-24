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

/// The before-landing settlement fix steps whose failure preserves a settled
/// reviewer repair: owner revalidation, and the later publication of a
/// validated repair.
const FAILED_LANDING_STEPS: [&str; 2] = [VALIDATE, LANDING_PUSH];

/// The settled reviewer repair's content in [`failed_landing_fixture`].
const REPAIR: &str = "landing review repair\n";

/// A before-landing review settled a reviewer fix R on `published` P, then
/// `failed` stopped the run: owner revalidation (`landing_review_validate`)
/// refused R, or R validated and its `landing_push` failed. PR 17 and
/// `origin/<branch>` stay at P, and `pr_failure_handoff` preserved R on
/// `refs/orbit/candidates/<task>/<run>`.
fn failed_landing_fixture(failed: &str) -> (Fixture, PathBuf, String, String) {
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
    std::fs::write(checkout.join("review.txt"), REPAIR).unwrap();
    git(&checkout, &["add", "review.txt"]);
    git(
        &checkout,
        &["commit", "-qm", "review: settled fix before landing"],
    );
    let reviewed = git(&checkout, &["rev-parse", "HEAD"]);

    let settle_output = json!({
        "gate": "passed",
        "reviewer_fixed": true,
        "implementation_head_sha": published,
        "reviewed_head_sha": reviewed,
        "reviewed_base_sha": f.base,
    });
    published_owner::checkpoint(&f, &f.source.run_id, 8, SETTLE, settle_output);
    // Owner revalidation that failed records no checkpoint.
    if failed == LANDING_PUSH {
        let validate_output = json!({
            "phase": "validate",
            "decision": "passed",
            "tested_head": reviewed,
            "branch": f.branch,
        });
        published_owner::checkpoint(&f, &f.source.run_id, 9, VALIDATE, validate_output);
    }
    f.state = f.runtime.read_run_state(&f.source.run_id).unwrap().unwrap();

    let (activity, message) = if failed == VALIDATE {
        (
            "candidate_validate",
            "candidate validation failed: cargo test exited with status 101",
        )
    } else {
        (
            "git_push",
            "git push failed: remote rejected temporary hook",
        )
    };
    let expected_ref = format!("refs/orbit/candidates/{}/{}", f.task, f.source.run_id);
    let handoff = f.action(
        "pr_failure_handoff",
        json!({
            "run_id": f.source.run_id,
            "failed_step_id": failed,
            "activity_name": activity,
            "error_code": "execution",
            "error_message": message,
            "job_input": f.source.input,
            "pipeline": f.state.pipeline,
        }),
    );
    assert_eq!(handoff["decision"], "landing_review_failure", "{handoff}");
    assert_eq!(handoff["failed_step_id"], failed, "{handoff}");
    assert_eq!(handoff["reviewer_push"], Value::Null, "{handoff}");
    let preservation = &handoff["reviewer_preservation"];
    assert_eq!(preservation["decision"], "preserved", "{handoff}");
    assert_eq!(preservation["commit_sha"], reviewed, "{handoff}");
    assert_eq!(preservation["published_head_sha"], published, "{handoff}");
    assert_eq!(preservation["ref"], expected_ref, "{handoff}");
    assert!(
        git(&f.repo, &["ls-remote", "origin", &f.branch]).starts_with(&published),
        "{failed}: origin PR branch stays at published head P"
    );
    assert!(
        git(&f.repo, &["ls-remote", "origin", &expected_ref]).starts_with(&reviewed),
        "{failed}: settled fix commit R is reachable on durable ref"
    );

    escalate(&f, &f.source.run_id);
    let mut state = f.runtime.read_run_state(&f.source.run_id).unwrap().unwrap();
    state.record_failure_activity("pr_failure_handoff".into(), failed.into(), handoff);
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

/// A failed revalidation or `landing_push` whose PR and origin branch remained
/// at `published` P admits its preserved reviewer repair R through explicit
/// `source_repair` (`inspect` + `submit`) and applies R onto the successor
/// checkout, including when the local branch in `repo` stayed at P.
fn a_failed_landing_source_repair_admits_and_applies_preserved_fix() {
    for failed in FAILED_LANDING_STEPS {
        let (f, checkout, published, reviewed) = failed_landing_fixture(failed);
        let expected_ref = format!("refs/orbit/candidates/{}/{}", f.task, f.source.run_id);
        // Simulate a run whose worktree detached or left `refs/heads/{branch}`
        // at P while R lives on the durable candidate ref.
        git(&checkout, &["checkout", "--quiet", "--detach"]);
        force_branch(&f.repo, &f.branch, &published, false);

        let inspection = f
            .runtime
            .run_tool_as_human("orbit.workflow.run.resume", f.request("inspect"))
            .unwrap();
        assert_eq!(inspection["binding"]["head_sha"], published, "{failed}");
        assert_eq!(
            inspection["binding"]["candidate_head_sha"], reviewed,
            "{failed}"
        );
        assert_eq!(
            inspection["binding"]["candidate_ref"], expected_ref,
            "{failed}"
        );

        let mut submit = f.submit(inspection["binding"].clone());
        submit["source_repair"]["policy_digest"] = inspection["policy_digest"].clone();
        submit["source_repair"]["unmet_acceptance"] =
            json!("the settled before-landing repair did not land; carry the preserved repair.");
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
        assert_eq!(
            resumed["outcome"], "resumed_repaired",
            "{failed}: {resumed}"
        );
        assert_eq!(resumed["reused_head_sha"], published, "{failed}");
        assert_eq!(resumed["source_sha"], reviewed, "{failed}");
        assert_eq!(resumed["reused_branch"], f.branch, "{failed}");
        assert_eq!(
            std::fs::read_to_string(next_checkout.join("review.txt")).unwrap(),
            REPAIR,
            "{failed}"
        );
    }
}

/// The incident: a green requeue after the settled reviewer fix failed owner
/// revalidation (or, once validated, its `landing_push`) resumes PR 17 at P
/// as the lease and carries the preserved fix commit R onto the successor
/// checkout, whose own pipeline validates R afresh.
fn a_failed_landing_requeue_resumes_the_preserved_fix_commit() {
    for failed in FAILED_LANDING_STEPS {
        let (f, _, published, reviewed) = failed_landing_fixture(failed);
        let expected_ref = format!("refs/orbit/candidates/{}/{}", f.task, f.source.run_id);

        let answer = requeue(&f, false).unwrap();
        assert_eq!(answer["status"], "backlog", "{failed}: {answer}");
        assert_eq!(answer["resumes"]["pull_request"], "17", "{failed}");
        assert_eq!(answer["resumes"]["head_sha"], published, "{failed}");
        assert_eq!(
            answer["resumes"]["candidate_head_sha"], reviewed,
            "{failed}"
        );
        assert_eq!(answer["resumes"]["candidate_ref"], expected_ref, "{failed}");

        let (run, setup) = super::published_owner::next(&f);
        let next_checkout = PathBuf::from(setup["workspace_path"].as_str().unwrap());
        let resumed = f.action(
            "candidate_resume",
            json!({"job_run_id":run.run_id,"task_ids":[f.task],
            "workspace_path":next_checkout,"base_sha":setup["base_sha"]}),
        );
        assert_eq!(resumed["outcome"], "resumed_carried", "{failed}: {resumed}");
        assert_eq!(resumed["reused_head_sha"], published, "{failed}");
        assert_eq!(resumed["source_sha"], reviewed, "{failed}");
        assert_eq!(
            std::fs::read_to_string(next_checkout.join("review.txt")).unwrap(),
            REPAIR,
            "{failed}"
        );
    }
}

/// A completed `landing_push`, changed PR/origin/local lease, mismatched
/// preservation receipt or, after failed revalidation, an unsettled or
/// validated correction stays refused with nothing written.
fn failed_landing_keeps_completed_lease_and_receipt_refusals() {
    for failed in FAILED_LANDING_STEPS {
        failed_landing_refusals(failed);
    }
}

fn failed_landing_refusals(failed: &str) {
    let (f, checkout, published, reviewed) = failed_landing_fixture(failed);
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
        let case = format!("{failed}/{case}");
        let requeue_err = refused(f, false, &case);
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
    let edit_state = |f: &Fixture, edit: &dyn Fn(&mut orbit_types::workflow::PipelineState)| {
        let mut state = f.runtime.read_run_state(&f.source.run_id).unwrap().unwrap();
        edit(&mut state);
        f.runtime.write_run_state(&f.source.run_id, &state).unwrap();
    };

    // 1. Completed landing_push (output present and step succeeded, or either alone).
    for mode in ["both", "output_only", "step_only"] {
        let output = json!({
            "phase": "push",
            "branch": branch,
            "local_sha": reviewed,
            "remote_sha_before": published,
        });
        edit_state(&f, &|state| {
            if mode != "step_only" {
                state.record_pipeline_output(LANDING_PUSH, output.clone());
            }
            if mode != "output_only" {
                state.record_step(10, JobRunState::Success, Some(output.clone()), None);
            }
        });
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

    // 4. Failed revalidation needs the settled correction it failed to
    // certify, and never a successful revalidation of it.
    if failed == VALIDATE {
        let settle = f.state.pipeline.get(SETTLE).unwrap().clone();
        let mut unfixed = settle.clone();
        unfixed["reviewer_fixed"] = json!(false);
        let mut elsewhere = settle.clone();
        elsewhere["implementation_head_sha"] = json!(reviewed);
        for (case, output) in [
            ("settlement_not_a_reviewer_fix", unfixed),
            ("settlement_of_another_implementation", elsewhere),
        ] {
            edit_state(&f, &|state| {
                state.record_step(8, JobRunState::Success, Some(output.clone()), None);
                state.record_pipeline_output(SETTLE, output.clone());
            });
            assert_both_refuse(&f, case);
            restore(&f);
        }
        let validated = json!({
            "phase": "validate",
            "decision": "passed",
            "tested_head": reviewed,
            "branch": branch,
        });
        edit_state(&f, &|state| {
            state.record_step(9, JobRunState::Success, Some(validated.clone()), None);
        });
        assert_both_refuse(&f, "revalidation_recorded_successful");
        restore(&f);
    }

    // Confirm the uncorrupted fixture still admits after all refusal checks.
    let inspection = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", f.request("inspect"))
        .unwrap();
    assert_eq!(inspection["binding"]["head_sha"], published, "{failed}");
    assert_eq!(
        inspection["binding"]["candidate_head_sha"], reviewed,
        "{failed}"
    );
}

/// A successful review-less publication through `watch_checks` whose PR then
/// goes red admits `source_repair` `inspect` even after terminal compaction
/// cleared `skipped_steps` (and when `landing_push` never ran), while a real
/// later publication without a settled reviewer correction stays refused.
fn review_less_success_after_watch_checks_admits_inspect_and_refuses_real_later_push() {
    let mut f = Fixture::new();
    let activities = f._root.path().join("global/resources/activities");
    for name in [
        "review_gate_admit",
        "agent_review_repair",
        "invoke_detached",
    ] {
        std::fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("assets/activities/{name}.yaml")),
            activities.join(format!("{name}.yaml")),
        )
        .unwrap();
    }
    let job_path = f
        ._root
        .path()
        .join("global/resources/jobs/task_pr_pipeline.yaml");
    let yaml = std::fs::read_to_string(&job_path).unwrap();
    std::fs::write(
        &job_path,
        yaml + &format!(
            "    - {{id: landing_review_gate_admit, target: 'activity:review_gate_admit'}}\n\
             \x20   - {{id: landing_review, target: 'activity:agent_review_repair', when: '{{{{ steps.landing_review_gate_admit.output.applies }}}} == true'}}\n\
             \x20   - {{id: {SETTLE}, target: 'activity:review_gate_settle'}}\n\
             \x20   - {{id: {VALIDATE}, target: 'activity:candidate_validate', when: '{{{{ steps.{SETTLE}.output.reviewer_fixed }}}} == true'}}\n\
             \x20   - {{id: {LANDING_PUSH}, target: 'activity:git_push', when: '{{{{ steps.{SETTLE}.output.reviewer_fixed }}}} == true'}}\n\
             \x20   - {{id: watch_checks, target: 'activity:invoke_detached'}}\n"
        ),
    )
    .unwrap();

    let mut run = f.runtime.show_job_run_observed(&f.source.run_id).unwrap();
    run.input = Some(json!({
        "task_ids": [f.task],
        "completion": "review",
        "auto_fix": false,
    }));
    run.state = JobRunState::Success;
    run.finished_at = Some(Utc::now());
    save_run(&f, &run);
    f.source = f.runtime.show_job_run_observed(&run.run_id).unwrap();

    let mut state = f.runtime.read_run_state(&run.run_id).unwrap().unwrap();
    state.initial_input = run.input.clone().unwrap();
    let steps = [
        (
            8,
            "landing_review_gate_admit",
            json!({"applies": false}),
            false,
        ),
        (9, "landing_review", Value::Null, true),
        (
            10,
            SETTLE,
            json!({"gate": "not_required", "reviewer_fixed": false}),
            false,
        ),
        (11, VALIDATE, Value::Null, true),
        (12, LANDING_PUSH, Value::Null, true),
        (13, "watch_checks", json!({"run_id": "jrun-watch"}), false),
    ];
    for (index, id, output, skipped) in steps {
        state.record_step(index, JobRunState::Success, Some(output.clone()), None);
        state.record_step_skipped(index, skipped);
        state.record_pipeline_output(id, output);
    }
    assert!(state.compact_for_terminal(JobRunState::Success));
    assert!(state.skipped_steps.is_empty());
    f.runtime.write_run_state(&run.run_id, &state).unwrap();
    f.state = state;

    // The PR went red after `watch_checks`.
    let mut answer: Value = serde_json::from_slice(&std::fs::read(&f.answer).unwrap()).unwrap();
    answer["statusCheckRollup"] = json!([
        {"name": "Check", "status": "COMPLETED", "conclusion": "FAILURE"}
    ]);
    std::fs::write(&f.answer, answer.to_string()).unwrap();

    // 1. Compacted review-less run (`skipped_steps` empty, `landing_push` checkpointed
    //    as `Success` with `Value::Null`) admits `inspect`.
    let inspection = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", f.request("inspect"))
        .expect("review-less success through watch_checks must admit inspect");
    assert_eq!(inspection["binding"]["head_sha"], f.head);
    assert_eq!(inspection["binding"]["pull_request"], "17");
    assert_eq!(inspection["binding"]["branch"], f.branch);
    assert!(inspection["policy_digest"].is_string());

    // 2. Uncompacted `Success` + `Value::Null` without a `skipped_steps` entry,
    //    and `landing_push` never run at all, also admit `inspect`.
    let mut uncompacted_null = f.state.clone();
    uncompacted_null.step_output_pointers.remove(&12);
    uncompacted_null.record_step(12, JobRunState::Success, Some(Value::Null), None);
    uncompacted_null.record_pipeline_output(LANDING_PUSH, Value::Null);
    f.runtime
        .write_run_state(&run.run_id, &uncompacted_null)
        .unwrap();
    let uncompacted_inspection = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", f.request("inspect"))
        .expect("uncompacted null landing_push without skipped_steps must admit inspect");
    assert_eq!(uncompacted_inspection["binding"]["head_sha"], f.head);

    let mut not_run = f.state.clone();
    not_run.step_states.remove(&12);
    not_run.step_outputs.remove(&12);
    not_run.step_output_pointers.remove(&12);
    not_run
        .pipeline
        .as_object_mut()
        .unwrap()
        .remove(LANDING_PUSH);
    f.runtime.write_run_state(&run.run_id, &not_run).unwrap();
    let not_run_inspection = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", f.request("inspect"))
        .expect("unreached landing_push must admit inspect");
    assert_eq!(not_run_inspection["binding"]["head_sha"], f.head);

    // 3. A real later publication (non-null output, or `Success` with missing
    //    output) without a settled reviewer correction stays refused.
    let mut real_push = f.state.clone();
    let push_output = json!({
        "phase": "push",
        "decision": "reused_current",
        "branch": f.branch,
        "local_sha": f.head,
        "remote_sha_before": f.head,
        "force_with_lease": true,
    });
    real_push.step_output_pointers.remove(&12);
    real_push.record_step(12, JobRunState::Success, Some(push_output.clone()), None);
    real_push.record_pipeline_output(LANDING_PUSH, push_output);
    f.runtime.write_run_state(&run.run_id, &real_push).unwrap();
    let real_push_err = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", f.request("inspect"))
        .expect_err("real later publication without settled correction must be refused")
        .to_string();
    assert!(
        real_push_err.contains("later publication lacks an authenticated reviewer correction"),
        "{real_push_err}"
    );

    let mut missing_output = f.state.clone();
    missing_output.step_output_pointers.remove(&12);
    missing_output.step_outputs.remove(&12);
    missing_output
        .pipeline
        .as_object_mut()
        .unwrap()
        .remove(LANDING_PUSH);
    f.runtime
        .write_run_state(&run.run_id, &missing_output)
        .unwrap();
    let missing_output_err = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", f.request("inspect"))
        .expect_err("Success landing_push with missing output must be refused")
        .to_string();
    assert!(
        missing_output_err.contains("later publication lacks an authenticated reviewer correction"),
        "{missing_output_err}"
    );
}

crate::scenario_table!(
    reviewer_push_scenarios,
    a_held_incomplete_review_requeue_resumes_the_attested_reviewer_head,
    unattested_or_moved_reviewer_heads_stay_refused,
    a_changed_specification_refuses_the_reviewer_head,
    a_rejected_landing_review_requeue_resumes_the_preserved_fix_commit,
    a_failed_landing_source_repair_admits_and_applies_preserved_fix,
    a_failed_landing_requeue_resumes_the_preserved_fix_commit,
    failed_landing_keeps_completed_lease_and_receipt_refusals,
    review_less_success_after_watch_checks_admits_inspect_and_refuses_real_later_push
);
