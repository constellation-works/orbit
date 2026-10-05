//! Desktop completion of a follower's delivery on the owner [ORB-14175].
//!
//! The follower's leaf runs and hands off pull request #42; the owner holds
//! the claim and the accepted handoff but never the leaf's run, which lives in
//! the follower's job store. Completion reads the owner-held evidence for that
//! exact host and run, takes the pull request's identity from the handoff (the
//! task carries no PR reference), and reads it from the provider by number.

use super::*;

use orbit_core::application::review::ExpectedCandidate;
use orbit_types::desktop::{
    DesktopCriterionOutcome, DesktopReviewDecision, DesktopReviewVerdict, DesktopTaskOperation,
    DesktopTaskRequest,
};
use orbit_types::task::TaskStatus;
use orbit_types::workflow::{
    REVIEW_GATE_ARTIFACT, ReviewBudget, ReviewCertificate, ReviewConsumption, ReviewValidation,
    ReviewVerdict, ReviewerIdentity, ValidationOutcome, ValidationRole,
};

const PR_URL: &str = "https://github.com/owner/repository/pull/42";

fn session(capability: McpCapability) -> ToolSessionContext {
    ToolSessionContext {
        effective_capabilities: BTreeSet::from([capability]),
        ..ToolSessionContext::default()
    }
}

/// A stand-in `gh` that answers only the exact view of pull request #42, from
/// `$HOME/pr.json`. A listing, or any other pull request, fails.
fn provider_answers_only_pr_42() {
    let home = PathBuf::from(std::env::var("HOME").unwrap());
    std::fs::create_dir_all(home.join("bin")).unwrap();
    let gh = home.join("bin/gh");
    std::fs::write(
        &gh,
        "#!/bin/sh\n[ \"$1 $2 $3\" = \"pr view 42\" ] || { echo \"unexpected: gh $*\" >&2; exit 1; }\n\
         exec cat \"$HOME/pr.json\"\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// The provider's current answer for pull request #42.
fn pull_request_is(state: &str, head: &str, base: &str) {
    let home = PathBuf::from(std::env::var("HOME").unwrap());
    std::fs::write(
        home.join("pr.json"),
        json!({
            "number": 42,
            "state": state,
            "mergedAt": if state == "MERGED" { json!("2026-10-05T06:50:00Z") } else { Value::Null },
            "headRefName": "orbit/fixture",
            "headRefOid": head,
            "baseRefName": base,
            "url": PR_URL,
        })
        .to_string(),
    )
    .unwrap();
}

fn verdict(task: &Value, head: &str, run: &str) -> DesktopReviewVerdict {
    let criteria = task["acceptance_criteria"]
        .as_array()
        .unwrap()
        .iter()
        .map(|criterion| DesktopCriterionOutcome {
            criterion: criterion.as_str().unwrap().into(),
            met: true,
            evidence: vec![PR_URL.into()],
        })
        .collect();
    DesktopReviewVerdict {
        decision: DesktopReviewDecision::Accept,
        rationale: "The merged pull request delivers the criteria.".into(),
        criteria,
        evidence: vec!["execution_summary".into(), PR_URL.into()],
        expected_run_id: Some(run.into()),
        expected_head: Some(head.into()),
    }
}

fn review(
    id: &str,
    revision: &str,
    verdict: DesktopReviewVerdict,
    request: &str,
) -> DesktopTaskRequest {
    DesktopTaskRequest {
        request_id: request.into(),
        operation: DesktopTaskOperation::Review {
            id: id.into(),
            expected_revision: revision.into(),
            verdict,
            complete: true,
        },
    }
}

/// The ordinary transitions an operator takes from `blocked` back to review.
fn back_to_review(owner: &OrbitRuntime, task: &str, summary: Option<&str>) {
    owner
        .run_tool(
            "orbit.task.update",
            json!({"id": task, "status": "in-progress", "model": "codex"}),
        )
        .expect("resume");
    let mut update = json!({"id": task, "status": "review", "model": "codex"});
    if let Some(summary) = summary {
        update["execution_summary"] = json!(summary);
    }
    owner
        .run_tool("orbit.task.update", update)
        .expect("back to review");
}

/// A passed review certificate with complete validation for `head`, recorded
/// in the owner's review store and on the task, as the review gate leaves it.
fn certify_final_head(owner: &OrbitRuntime, repo: &Path, task: &str, head: &str) {
    let revision = |commit: &str| SourceRevision {
        commit: commit.into(),
        tree: "f".repeat(40),
    };
    let certificate = ReviewCertificate {
        schema_version: 1,
        attempt_id: "rva-final-head".into(),
        lineage_key: "lineage-final-head".into(),
        task_ids: vec![task.into()],
        task_meaning_digest: "0".repeat(64),
        repository: "owner/repository".into(),
        base: revision(&"c".repeat(40)),
        reviewed_candidate: revision(head),
        final_candidate: revision(head),
        implementation_commits: vec![],
        repair_commits: vec![],
        verdict: ReviewVerdict::Accept,
        assurance: None,
        findings: vec![],
        validation: vec![ReviewValidation {
            command: "make ci-fast".into(),
            outcome: ValidationOutcome::Passed,
            role: ValidationRole::Required,
            note: None,
            check: None,
        }],
        validation_complete: true,
        reviewer: ReviewerIdentity {
            crew: "sol".into(),
            provider: "codex".into(),
            model: "fixture".into(),
            reasoning_effort: None,
            implementer_model: None,
            same_model_as_implementer: false,
        },
        consumed: ReviewConsumption { seconds: 60 },
        budget: ReviewBudget::default(),
        escalation: None,
        selectors_widened: vec![],
        issued_at: Utc::now(),
    };
    owner
        .review_store()
        .unwrap()
        .review_certificate_record(&owner.workspace_id().unwrap(), &certificate)
        .unwrap();
    let source = repo.join(".orbit/tmp").join(REVIEW_GATE_ARTIFACT);
    std::fs::create_dir_all(source.parent().unwrap()).unwrap();
    std::fs::write(&source, serde_json::to_vec(&certificate).unwrap()).unwrap();
    owner
        .run_tool(
            "orbit.task.artifact.put",
            json!({"id": task, "model": "codex", "path": REVIEW_GATE_ARTIFACT, "source_path": source}),
        )
        .expect("certificate artifact");
}

/// The recovered Mac delivery behind ORB-14130: the follower handed off PR
/// #42, the owner's landing stopped, an operator merged the PR by hand,
/// revoked the handoff and recovered the claim, then returned the task to
/// review. Completion rests on the claim and handoff the owner holds for that
/// exact host and run, the PR the handoff names, read by number, and evidence
/// for the head that merged.
#[test]
fn a_recovered_follower_delivery_completes_from_owner_held_evidence() {
    if !isolated(
        module_path!(),
        "a_recovered_follower_delivery_completes_from_owner_held_evidence",
    ) {
        return;
    }
    provider_answers_only_pr_42();
    let pair = Pair::new(1);
    let owner = &pair.wire.owner;
    *pair.wire.accept_handoffs.lock().unwrap() = true;
    let drain = pair.run_drain();
    let leaf = pair.running_leaf(&drain, 1);
    let task = pair.claimed_task(&leaf);
    pair.leaf_hands_off(&leaf);
    let settled = pair.pass(&drain);
    assert!(settled["error"].is_null(), "{settled}");
    assert_eq!(pair.owner_status(&task), "review");

    let operator = session(McpCapability::Operator);
    let candidate = "a".repeat(40);
    let console = owner.distributed_claim_console().unwrap();
    let claim = console["claims"][0].clone();
    let landing = claim["handoff"]["candidate"]["landing_branch"]
        .as_str()
        .unwrap()
        .to_string();
    pull_request_is("MERGED", &candidate, &landing);

    // While the handoff holds its claim, the snapshot names the supported way
    // forward rather than a status change the claim would refuse.
    let held = owner.desktop_task_snapshot(&task, &operator).unwrap();
    assert!(!held.actions.complete.enabled);
    let reason = held.actions.complete.reason.unwrap_or_default();
    assert!(
        reason.contains("revoke the handoff and recover the claim"),
        "{reason}"
    );

    let handoff_id = claim["handoff"]["handoff_id"].as_str().unwrap();
    let expected = ExpectedCandidate {
        candidate_commit: candidate.clone(),
        base_commit: "c".repeat(40),
    };
    owner
        .revoke_handoff_as_operator(
            handoff_id,
            &expected,
            "operator",
            "merged by hand",
            "revoke",
        )
        .expect("revoke");
    owner
        .recover_claim_as_operator(
            claim["claim_id"].as_str().unwrap(),
            "handed_off",
            TaskStatus::Blocked,
            "operator",
            "landing stopped on a conflict; merged by hand",
            "recover",
        )
        .expect("recover");
    back_to_review(owner, &task, None);

    let shown = pair.owner_task(&task);
    assert_eq!(shown["job_run_id"], leaf.as_str(), "{shown:#}");
    assert_eq!(
        shown["job_run_machine"]["machine_id"], FOLLOWER,
        "{shown:#}"
    );
    assert_eq!(shown["external_refs"], json!([]), "{shown:#}");

    // An open pull request does not complete a handed-off delivery, and the
    // head is read by number: the stand-in provider refuses any listing.
    pull_request_is("OPEN", &candidate, &landing);
    let open = owner.desktop_task_snapshot(&task, &operator).unwrap();
    assert_eq!(open.reviewed_head.as_deref(), Some(candidate.as_str()));
    assert!(open.actions.review.enabled, "{:?}", open.actions.review);
    assert!(!open.actions.complete.enabled);
    assert!(
        open.actions
            .complete
            .reason
            .as_deref()
            .unwrap_or_default()
            .contains("has not merged"),
        "{:?}",
        open.actions.complete
    );

    // A head changed after the handoff inherits none of its candidate's
    // evidence, in the snapshot and in the write alike.
    let manual = "e".repeat(40);
    pull_request_is("MERGED", &manual, &landing);
    let changed = owner.desktop_task_snapshot(&task, &operator).unwrap();
    assert_eq!(changed.reviewed_head.as_deref(), Some(manual.as_str()));
    assert!(!changed.actions.complete.enabled);
    let refusal = changed.actions.complete.reason.clone().unwrap_or_default();
    assert!(
        refusal.contains("do not carry to a changed head"),
        "{refusal}"
    );
    let refused = owner
        .desktop_task_write(
            review(
                &task,
                &changed.revision,
                verdict(&shown, &manual, &leaf),
                "stale-head",
            ),
            None,
            None,
            &operator,
        )
        .expect_err("a changed head cannot complete on the candidate's evidence");
    assert!(
        refused
            .to_string()
            .contains("do not carry to a changed head"),
        "{refused}"
    );

    // Completion stays an operator decision.
    let agent = session(McpCapability::Agent);
    assert!(
        !owner
            .desktop_task_snapshot(&task, &agent)
            .unwrap()
            .actions
            .complete
            .enabled
    );

    // Fresh evidence for the merged head lets the operator complete.
    certify_final_head(owner, &pair.owner_repo, &task, &manual);
    let certified = owner.desktop_task_snapshot(&task, &operator).unwrap();
    assert!(
        certified.actions.complete.enabled,
        "{:?}",
        certified.actions.complete
    );
    assert!(
        owner
            .desktop_task_write(
                review(
                    &task,
                    &certified.revision,
                    verdict(&shown, &manual, &leaf),
                    "agent"
                ),
                None,
                None,
                &agent,
            )
            .is_err(),
        "an agent session cannot complete"
    );
    let request = review(
        &task,
        &certified.revision,
        verdict(&shown, &manual, &leaf),
        "complete",
    );
    let done = owner
        .desktop_task_write(request.clone(), None, None, &operator)
        .expect("evidence-bound completion");
    assert!(!done.replayed);
    assert_eq!(done.snapshot.task.status, TaskStatus::Done);
    let replay = owner
        .desktop_task_write(request, None, None, &operator)
        .expect("replay");
    assert!(replay.replayed);

    // Provenance and the retained records stay truthful.
    let completed = pair.owner_task(&task);
    assert_eq!(completed["job_run_id"], leaf.as_str());
    assert_eq!(completed["job_run_machine"]["machine_id"], FOLLOWER);
    let audit = comments_of(&completed);
    assert!(
        audit.contains(&format!("execution_machine={FOLLOWER}")),
        "{audit}"
    );
    assert!(audit.contains(&format!("pull_request={PR_URL}")), "{audit}");
    assert_eq!(
        completed["history"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|entry| entry["event"] == "desktop_mutation")
            .count(),
        1,
        "{completed:#}"
    );
    let retained = owner.distributed_claim_console().unwrap();
    assert_eq!(retained["claims"][0]["phase"], "revoked");
    assert_eq!(
        retained["claims"][0]["handoff"]["handoff_id"].as_str(),
        Some(handoff_id)
    );
}

/// A claim recovered while its leaf was still running proves nothing about
/// that run, and a run the owner happens to hold under the same id is a
/// different run: completion refuses instead of substituting it.
#[test]
fn a_same_named_owner_run_never_stands_in_for_a_follower_run() {
    if !isolated(
        module_path!(),
        "a_same_named_owner_run_never_stands_in_for_a_follower_run",
    ) {
        return;
    }
    let pair = Pair::new(1);
    let owner = &pair.wire.owner;
    let drain = pair.run_drain();
    let leaf = pair.running_leaf(&drain, 1);
    let task = pair.claimed_task(&leaf);
    let operator = session(McpCapability::Operator);

    // A live claim fences the task, and its reason says how to recover it.
    let live = owner.desktop_task_snapshot(&task, &operator).unwrap();
    assert!(
        live.actions
            .complete
            .reason
            .as_deref()
            .unwrap_or_default()
            .contains("recover the claim"),
        "{:?}",
        live.actions.complete
    );

    let claim = owner.distributed_claim_console().unwrap()["claims"][0].clone();
    owner
        .recover_claim_as_operator(
            claim["claim_id"].as_str().unwrap(),
            "running",
            TaskStatus::Blocked,
            "operator",
            "the follower went away",
            "recover",
        )
        .expect("recover");

    // The owner's own job store gains a successful run with the leaf's id.
    let jobs = orbit_store::compose::workspace_job_run_store(
        owner.sqlite_store().unwrap(),
        owner.workspace_id().unwrap(),
    );
    let local = jobs
        .insert_job_run(LEAF_JOB, 1, Utc::now(), None, None)
        .unwrap()
        .run_id;
    let store = owner.sqlite_store().unwrap();
    let workspace_id = owner.workspace_id().unwrap();
    store
        .with_transaction(|tx| {
            tx.connection()
                .execute(
                    "UPDATE job_runs SET run_id = ?1 WHERE workspace_id = ?2 AND run_id = ?3",
                    [leaf.as_str(), workspace_id.as_str(), local.as_str()],
                )
                .unwrap();
            Ok(())
        })
        .unwrap();
    jobs.mark_job_run_running(&leaf, Utc::now(), std::process::id())
        .unwrap();
    jobs.finalize_job_run(&leaf, JobRunState::Success, Utc::now(), None)
        .unwrap();
    assert_eq!(
        jobs.get_job_run(&leaf).unwrap().unwrap().state,
        JobRunState::Success
    );

    back_to_review(owner, &task, Some("Recovered."));
    let snapshot = owner.desktop_task_snapshot(&task, &operator).unwrap();
    assert!(!snapshot.actions.complete.enabled);
    let reason = snapshot.actions.complete.reason.clone().unwrap_or_default();
    assert!(
        reason.contains("recovered before the run handed off"),
        "{reason}"
    );
    let shown = pair.owner_task(&task);
    let mut no_pr = verdict(&shown, "", &leaf);
    no_pr.expected_head = None;
    no_pr.evidence = vec!["execution_summary".into()];
    for criterion in &mut no_pr.criteria {
        criterion.evidence = vec!["execution_summary".into()];
    }
    let refused = owner
        .desktop_task_write(
            review(&task, &snapshot.revision, no_pr, "substitute"),
            None,
            None,
            &operator,
        )
        .expect_err("the owner's same-named run is not the follower's");
    assert!(
        refused
            .to_string()
            .contains("recovered before the run handed off"),
        "{refused}"
    );
    assert_eq!(pair.owner_status(&task), "review");
}
