//! [ORB-14530] External review evidence counts only from a trusted writer. A
//! result an agent attached, even one planted for its own tree before review,
//! never reaches the reviewer as satisfied, never settles a review, and never
//! releases an evidence hold; an operator's result and log still do.

use orbit_core::TaskStatus;
use orbit_types::workflow::{
    JobRunState, REVIEW_EVIDENCE_HOLD_ARTIFACT, REVIEW_GATE_ARTIFACT, ReviewCertificate,
    ReviewEvidenceHold,
};
use serde_json::json;

use super::review_continuation::{
    attach, attach_as_operator, interrupted_report, manifest, run_review_pipeline,
};
use super::review_gate_audit::Fixture;

const COMMAND: &str = "scripts/codeql-rust-local.sh";
const RESULT: &str = "evidence/codeql-rust-linux.json";
const LOG: &str = "evidence/codeql-rust-linux.log.json";

fn git(fixture: &Fixture, args: &[&str]) -> String {
    let mut command = std::process::Command::new("git");
    orbit_common::test_env::clear_inherited_authority(|key| {
        command.env_remove(key);
    });
    let output = command
        .args(args)
        .current_dir(&fixture.repo)
        .output()
        .unwrap();
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

fn status(fixture: &Fixture) -> TaskStatus {
    fixture.runtime.get_task(&fixture.task_id).unwrap().status
}

fn last_event(fixture: &Fixture) -> String {
    fixture
        .runtime
        .get_task_history(&fixture.task_id)
        .unwrap()
        .last()
        .unwrap()
        .event
        .clone()
}

#[test]
fn agent_written_evidence_neither_reaches_the_reviewer_nor_settles_nor_releases_a_hold() {
    if !super::dispatch_admission::isolated(
        "review_evidence_writers::agent_written_evidence_neither_reaches_the_reviewer_nor_settles_nor_releases_a_hold",
    ) {
        return;
    }
    let mut fixture = Fixture::new_with_required_commands(&[COMMAND]);
    let tree = git(&fixture, &["rev-parse", "HEAD^{tree}"]);
    let planted = json!({
        "schema_version": 1, "attempt_id": "planted",
        "candidate": {"commit": git(&fixture, &["rev-parse", "HEAD"]), "tree": tree},
        "kind": "codeql", "name": "Linux CodeQL (rust)", "command": COMMAND,
        "outcome": "passed", "log_artifact": LOG,
    });
    // The implementer plants a passing result for its own tree before any
    // review, with no hold present.
    attach(&fixture, LOG, &json!({"output": "never ran"}));
    attach(&fixture, RESULT, &planted);

    fixture.admit();
    let input = manifest(&fixture);
    assert_eq!(
        input.candidate.tree, tree,
        "the planted result names the reviewed tree"
    );
    assert!(
        input.satisfied_external_evidence.is_empty(),
        "an agent-written result must not reach the reviewer as satisfied"
    );

    // A reviewer that cannot run the check names it. Settlement must not
    // reconcile the planted result into a passing verdict.
    let mut report = interrupted_report(&fixture);
    report["external_evidence"] = json!([{
        "kind": "codeql", "name": "Linux CodeQL (rust)", "command": COMMAND, "artifact": RESULT,
    }]);
    report["validation"].as_array_mut().unwrap().push(json!({
        "id": "V2", "command": COMMAND, "outcome": "not_run", "role": "required",
    }));
    fixture.put_report(&report);
    run_review_pipeline(&fixture);
    let certificate: ReviewCertificate = artifact(&fixture, REVIEW_GATE_ARTIFACT);
    assert!(
        !certificate.verdict.passed(),
        "an agent-written result must not settle the review: {:?}",
        certificate.verdict
    );
    let hold: ReviewEvidenceHold = artifact(&fixture, REVIEW_EVIDENCE_HOLD_ARTIFACT);
    assert_eq!(
        fixture.runtime.show_job_run(&hold.run_id).unwrap().state,
        JobRunState::Held
    );
    assert_eq!(last_event(&fixture), "review_awaiting_evidence");

    // Re-putting the result under the hold does not release it.
    let mut result = planted.clone();
    result["attempt_id"] = json!(hold.attempt_id);
    result["candidate"] = serde_json::to_value(&hold.candidate).unwrap();
    attach(&fixture, RESULT, &result);
    assert_eq!(status(&fixture), TaskStatus::InProgress);
    assert_eq!(last_event(&fixture), "review_awaiting_evidence");

    // An operator's result beside the agent's log is still not a pair.
    attach_as_operator(&fixture, RESULT, &result);
    assert_eq!(
        status(&fixture),
        TaskStatus::InProgress,
        "a result whose log an agent wrote must not release the hold"
    );
    attach_as_operator(&fixture, LOG, &json!({"output": "operator ran it"}));
    assert_eq!(status(&fixture), TaskStatus::Backlog);
    assert_eq!(last_event(&fixture), "review_evidence_received");
}
