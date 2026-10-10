//! [ORB-14450] A candidate held on named external evidence is resumed after
//! the evidence arrives, and the evidence follows it across an unchanged
//! patch, through the runtime's stores and the real review gate.

use std::collections::HashMap;
use std::path::Path;

use chrono::Utc;
use orbit_core::TaskStatus;
use orbit_core::application::task::TaskUpdateParams;
use orbit_engine::{RuntimeHost, execute_deterministic_action};
use orbit_types::workflow::{
    JobRunState, REVIEW_EVIDENCE_HOLD_ARTIFACT, REVIEW_GATE_ARTIFACT, ReviewCertificate,
    ReviewEvidenceHold, ReviewVerdict,
};
use serde_json::{Value, json};

use super::review_continuation::{
    attach, attach_as_operator, interrupted_report, manifest, run_review_pipeline,
};
use super::review_gate_audit::Fixture;

fn git(repo: &Path, args: &[&str]) -> String {
    let mut command = std::process::Command::new("git");
    orbit_common::test_env::clear_inherited_authority(|key| {
        command.env_remove(key);
    });
    let output = command.args(args).current_dir(repo).output().unwrap();
    assert!(output.status.success(), "git {args:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn artifact<T: serde::de::DeserializeOwned>(fixture: &Fixture, path: &str) -> T {
    serde_json::from_slice(
        &fixture
            .runtime
            .get_task_artifact(&fixture.task_id, path)
            .unwrap()
            .unwrap()
            .content,
    )
    .unwrap()
}

/// Hold the fixture's candidate on one native check, then attach a passing
/// result and its log so the receipt requeues the task. Returns the hold and
/// the reviewer's report, to repeat under the next attempt.
fn hold_and_receive(fixture: &mut Fixture) -> (ReviewEvidenceHold, Value) {
    fixture.admit();
    let mut report = interrupted_report(fixture);
    report["external_evidence"] = json!([{
        "kind": "native_os", "name": "macOS run", "command": "native macos",
        "artifact": "evidence/macos.json",
    }]);
    report["validation"].as_array_mut().unwrap().push(json!({
        "id": "V2", "command": "native macos", "outcome": "not_run", "role": "required",
    }));
    fixture.put_report(&report);
    run_review_pipeline(fixture);
    let hold: ReviewEvidenceHold = artifact(fixture, REVIEW_EVIDENCE_HOLD_ARTIFACT);
    attach_as_operator(
        fixture,
        "evidence/macos.json",
        &json!({
            "schema_version": 1, "attempt_id": hold.attempt_id, "candidate": hold.candidate,
            "kind": "native_os", "name": "macOS run", "command": "native macos",
            "outcome": "passed", "log_artifact": "evidence/macos-log.json",
        }),
    );
    attach_as_operator(
        fixture,
        "evidence/macos-log.json",
        &json!({"output": "passed"}),
    );
    assert_eq!(
        fixture.runtime.get_task(&fixture.task_id).unwrap().status,
        TaskStatus::Backlog,
        "the receipt requeues the held task"
    );
    (hold, report)
}

/// The task's next delivery run, admitted and linked as worktree setup does.
fn next_run(fixture: &mut Fixture, hold: &ReviewEvidenceHold) -> String {
    let previous = fixture.runtime.show_job_run(&hold.run_id).unwrap();
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
    next.run_id
}

/// Check out `main` as the next run's clean base, resume the task's
/// candidate onto it, and commit what it applied as the commit step does.
fn resume_onto_main(fixture: &Fixture, hold: &ReviewEvidenceHold) -> Value {
    git(&fixture.repo, &["checkout", "--quiet", "--detach", "main"]);
    let base_sha = git(&fixture.repo, &["rev-parse", "HEAD"]);
    let resumed = execute_deterministic_action(
        &fixture.runtime,
        "candidate_resume",
        &json!({}),
        &json!({
            "job_run_id": fixture.input["job_run_id"],
            "task_ids": [fixture.task_id],
            "workspace_path": fixture.input["workspace_path"],
            "base_sha": base_sha,
            "prior_job_run_id": hold.run_id,
        }),
        false,
        &HashMap::new(),
        None,
    )
    .unwrap();
    assert_eq!(resumed["outcome"], "resumed_held", "{resumed}");
    assert_eq!(resumed["implement"], false, "no implementation step runs");
    assert_eq!(resumed["source_sha"], hold.candidate.commit.as_str());
    git(&fixture.repo, &["add", "-A"]);
    git(
        &fixture.repo,
        &["commit", "--quiet", "-m", "resumed candidate"],
    );
    resumed
}

fn tree(fixture: &Fixture, revision: &str) -> String {
    git(
        &fixture.repo,
        &["rev-parse", &format!("{revision}^{{tree}}")],
    )
}

fn commit_on_main(fixture: &Fixture, file: &str) -> String {
    let head = git(&fixture.repo, &["rev-parse", "HEAD"]);
    git(&fixture.repo, &["checkout", "--quiet", "main"]);
    std::fs::write(fixture.repo.join(file), "base moved\n").unwrap();
    git(&fixture.repo, &["add", file]);
    git(
        &fixture.repo,
        &["commit", "--quiet", "-m", "the base moves"],
    );
    let main = git(&fixture.repo, &["rev-parse", "HEAD"]);
    git(&fixture.repo, &["checkout", "--quiet", "--detach", &head]);
    main
}

/// Review the admitted candidate with `report` repeating the held check, as
/// a fresh reviewer that cannot run it does.
fn review(fixture: &Fixture, report: &mut Value) {
    report["attempt_id"] = fixture.input["admission"]["attempt_id"].clone();
    fixture.put_report(report);
    run_review_pipeline(fixture);
}

#[test]
fn an_evidence_receipt_resumes_the_held_candidate_and_its_review_finds_the_evidence() {
    if !super::dispatch_admission::isolated(
        "review_held_resume::an_evidence_receipt_resumes_the_held_candidate_and_its_review_finds_the_evidence",
    ) {
        return;
    }
    let mut fixture = Fixture::new_with_required_commands(&["native macos"]);
    let (hold, mut report) = hold_and_receive(&mut fixture);
    // A task pilot widens the selectors while the task waits in backlog.
    fixture
        .runtime
        .update_task_with_identity(
            &fixture.task_id,
            TaskUpdateParams {
                context_files: Some(vec!["file:candidate.txt".into(), "file:.gitignore".into()]),
                ..Default::default()
            },
            Some("codex".into()),
            None,
        )
        .unwrap();
    let next = next_run(&mut fixture, &hold);
    resume_onto_main(&fixture, &hold);
    assert_ne!(
        git(&fixture.repo, &["rev-parse", "HEAD"]),
        hold.candidate.commit
    );
    assert_eq!(
        tree(&fixture, "HEAD"),
        hold.candidate.tree,
        "on the hold's own base the resumed candidate is the held tree"
    );

    fixture.admit();
    let admission = &fixture.input["admission"];
    assert_eq!(admission["evidence_carry"], Value::Null, "{admission}");
    assert!(
        admission["task_selectors"][&fixture.task_id]
            .as_array()
            .unwrap()
            .contains(&json!("file:.gitignore")),
        "the resumed review reads the current selectors: {admission}"
    );
    let input = manifest(&fixture);
    assert_eq!(input.satisfied_external_evidence.len(), 1);
    assert_eq!(input.evidence_carried, None);

    review(&fixture, &mut report);
    assert_eq!(
        fixture.runtime.show_job_run(&next).unwrap().state,
        JobRunState::Success
    );
    assert_eq!(
        fixture
            .runtime
            .read_run_state(&next)
            .unwrap()
            .unwrap()
            .pipeline["review_gate_settle"]["gate"],
        "passed"
    );
    let still: ReviewEvidenceHold = artifact(&fixture, REVIEW_EVIDENCE_HOLD_ARTIFACT);
    assert_eq!(still.run_id, hold.run_id, "no second hold");
    let certificate: ReviewCertificate = artifact(&fixture, REVIEW_GATE_ARTIFACT);
    assert_eq!(certificate.verdict, ReviewVerdict::Accept);
    assert_eq!(certificate.final_candidate.tree, hold.candidate.tree);
    assert_eq!(certificate.evidence_carried, None);
}

#[test]
fn evidence_carries_across_a_rebase_only_while_the_patch_is_unchanged() {
    if !super::dispatch_admission::isolated(
        "review_held_resume::evidence_carries_across_a_rebase_only_while_the_patch_is_unchanged",
    ) {
        return;
    }
    let mut fixture = Fixture::new_with_required_commands(&["native macos"]);
    let (hold, mut report) = hold_and_receive(&mut fixture);
    commit_on_main(&fixture, "base.txt");
    let next = next_run(&mut fixture, &hold);
    resume_onto_main(&fixture, &hold);
    let resumed_tree = tree(&fixture, "HEAD");
    assert_ne!(resumed_tree, hold.candidate.tree);

    fixture.admit();
    let carry = fixture.input["admission"]["evidence_carry"].clone();
    assert_eq!(carry["outcome"], "carried", "{carry}");
    assert_eq!(carry["carried"]["from_tree"], hold.candidate.tree.as_str());
    assert_eq!(carry["carried"]["to_tree"], resumed_tree.as_str());
    assert!(!carry["carried"]["patch_id"].as_str().unwrap().is_empty());
    let input = manifest(&fixture);
    assert_eq!(
        serde_json::to_value(&input.evidence_carried).unwrap(),
        carry["carried"]
    );
    assert_eq!(input.satisfied_external_evidence.len(), 1);

    review(&fixture, &mut report);
    assert_eq!(
        fixture.runtime.show_job_run(&next).unwrap().state,
        JobRunState::Success
    );
    let certificate: ReviewCertificate = artifact(&fixture, REVIEW_GATE_ARTIFACT);
    assert_eq!(certificate.verdict, ReviewVerdict::Accept);
    assert_eq!(certificate.final_candidate.tree, resumed_tree);
    assert_eq!(
        serde_json::to_value(&certificate.evidence_carried).unwrap(),
        carry["carried"],
        "the report records the carry"
    );

    // Completion rebases the reviewed head cleanly onto a newer base and asks
    // for a re-review: the same rule carries the evidence again.
    let reviewed = git(&fixture.repo, &["rev-parse", "HEAD"]);
    let newer = commit_on_main(&fixture, "later.txt");
    git(&fixture.repo, &["rebase", "--quiet", "main"]);
    let rebased = git(&fixture.repo, &["rev-parse", "HEAD"]);
    assert_ne!(rebased, reviewed);
    let mut state = fixture.runtime.read_run_state(&next).unwrap().unwrap();
    state.pipeline["complete_pr"] = json!({
        "re_review_required": true,
        "rebased": {"head_sha": rebased, "base_sha": newer},
    });
    fixture.runtime.write_run_state(&next, &state).unwrap();
    let mut rereview = fixture.input.clone();
    rereview["re_review_after"] = json!("complete_pr");
    rereview["completion"] = json!("done");
    let admitted = fixture
        .runtime
        .run_deterministic(
            "review_gate_admit",
            &json!({}),
            &rereview,
            Default::default(),
        )
        .unwrap();
    assert_eq!(
        admitted["evidence_carry"]["outcome"], "carried",
        "{admitted}"
    );
    assert_eq!(
        admitted["evidence_carry"]["carried"]["from_tree"],
        hold.candidate.tree.as_str(),
        "the evidence is still the result checked on the held tree"
    );
    assert_eq!(
        admitted["evidence_carry"]["carried"]["to_tree"],
        tree(&fixture, "HEAD").as_str()
    );
    assert_eq!(manifest(&fixture).satisfied_external_evidence.len(), 1);

    // A completion rebase whose conflict resolution changed the patch does
    // not carry it.
    std::fs::write(fixture.repo.join("candidate.txt"), "after, resolved\n").unwrap();
    git(
        &fixture.repo,
        &["commit", "--quiet", "-am", "resolve conflict"],
    );
    let resolved = git(&fixture.repo, &["rev-parse", "HEAD"]);
    state.pipeline["complete_pr"]["rebased"]["head_sha"] = json!(resolved);
    fixture.runtime.write_run_state(&next, &state).unwrap();
    let admitted = fixture
        .runtime
        .run_deterministic(
            "review_gate_admit",
            &json!({}),
            &rereview,
            Default::default(),
        )
        .unwrap();
    assert_eq!(
        admitted["evidence_carry"],
        json!({
            "outcome": "rerequested", "reason": "patch_changed",
            "from_tree": hold.candidate.tree, "to_tree": tree(&fixture, "HEAD"),
        })
    );
    assert!(manifest(&fixture).satisfied_external_evidence.is_empty());
}

#[test]
fn evidence_an_agent_rewrote_does_not_carry_across_a_rebase() {
    if !super::dispatch_admission::isolated(
        "review_held_resume::evidence_an_agent_rewrote_does_not_carry_across_a_rebase",
    ) {
        return;
    }
    let mut fixture = Fixture::new_with_required_commands(&["native macos"]);
    let (hold, _) = hold_and_receive(&mut fixture);
    commit_on_main(&fixture, "base.txt");
    next_run(&mut fixture, &hold);
    resume_onto_main(&fixture, &hold);
    // [ORB-14530] The implementer re-puts the held tree's result unchanged:
    // the bytes are the operator's, but the latest writer is an agent.
    let result: Value = artifact(&fixture, "evidence/macos.json");
    attach(&fixture, "evidence/macos.json", &result);

    fixture.admit();
    assert_eq!(
        fixture.input["admission"]["evidence_carry"],
        Value::Null,
        "an agent-written result on the held tree must not carry"
    );
    assert!(manifest(&fixture).satisfied_external_evidence.is_empty());
}

#[test]
fn a_resumed_candidate_whose_patch_changed_is_held_again_for_its_own_evidence() {
    if !super::dispatch_admission::isolated(
        "review_held_resume::a_resumed_candidate_whose_patch_changed_is_held_again_for_its_own_evidence",
    ) {
        return;
    }
    let mut fixture = Fixture::new_with_required_commands(&["native macos"]);
    let (hold, mut report) = hold_and_receive(&mut fixture);
    let next = next_run(&mut fixture, &hold);
    resume_onto_main(&fixture, &hold);
    std::fs::write(fixture.repo.join("candidate.txt"), "after, and more\n").unwrap();
    git(
        &fixture.repo,
        &["commit", "--quiet", "-am", "implementer change"],
    );
    let changed_tree = tree(&fixture, "HEAD");

    fixture.admit();
    assert_eq!(
        fixture.input["admission"]["evidence_carry"],
        json!({
            "outcome": "rerequested", "reason": "patch_changed",
            "from_tree": hold.candidate.tree, "to_tree": changed_tree,
        })
    );
    assert!(manifest(&fixture).satisfied_external_evidence.is_empty());

    review(&fixture, &mut report);
    assert_eq!(
        fixture.runtime.show_job_run(&next).unwrap().state,
        JobRunState::Held
    );
    let again: ReviewEvidenceHold = artifact(&fixture, REVIEW_EVIDENCE_HOLD_ARTIFACT);
    assert_eq!(again.run_id, next);
    assert_eq!(again.candidate.tree, changed_tree);
}
