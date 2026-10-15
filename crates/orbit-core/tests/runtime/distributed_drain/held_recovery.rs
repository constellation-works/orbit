//! Real claim, Git and tool boundaries for prospective retained-candidate recovery.
use super::claimed_owed_evidence::{artifact, owner_checkout, publish_origin, record, report};
use super::claimed_review::{REVIEW_CREW, ReviewedLeaf, ToOwner};
use super::*;
use orbit_store::contracts::{ClaimCandidateRef, ClaimFailure};
use orbit_types::tool::WorkerInvocation;
use orbit_types::workflow::{
    HELD_RECOVERY_ADMITTED, HELD_RECOVERY_CLAIMED, HeldRecoveryMeaning,
    REVIEW_EVIDENCE_HOLD_ARTIFACT, REVIEW_GATE_ARTIFACT, ReviewCertificate, ReviewEvidenceHold,
    ReviewEvidenceKind, ReviewEvidenceRequirement, ReviewVerdict, ValidationOutcome,
};

const URL: &str = "https://github.com/owner/repository.git";
mod fixture;
use fixture::Fixture;

#[test]
fn approved_source_amendment_admits_one_claim_without_rewriting_rejected_evidence() {
    if !isolated(
        module_path!(),
        "approved_source_amendment_admits_one_claim_without_rewriting_rejected_evidence",
    ) {
        return;
    }
    let f = Fixture::new(false);
    f.advance_base();
    let owner = &f.leaf.pair.wire.owner;
    let rejected = f.leaf.owner_artifact(REVIEW_GATE_ARTIFACT).unwrap();
    let claim_before = f.leaf.pair.owner_claim_for_leaf(&f.leaf.leaf);
    assert!(
        owner
            .run_tool_as_human(
                "orbit.task.review_recover",
                json!({"id":f.leaf.task,"action":"inspect","disposition":"adjudicate_evidence"})
            )
            .is_err(),
        "failed assertion cannot be called unavailable external proof"
    );
    f.amendment();
    let unapproved = owner.kept_claim_candidate(&f.leaf.task).unwrap().unwrap();
    assert!(
        matches!(
            unapproved.fresh,
            Some((orbit_store::contracts::CandidateFreshReason::SpecChanged, _))
        ),
        "ordinary meaning changes still refuse kept-candidate reuse"
    );
    let inspected = f.inspect("source_repair");
    assert!(!inspected["binding"]["delta"].as_array().unwrap().is_empty());
    let payload = f.submit(&inspected["binding"]);
    let first = owner
        .run_tool_as_human("orbit.task.review_recover", payload.clone())
        .unwrap();
    assert_eq!(first["queued"], true);
    let replay = owner
        .run_tool_as_human("orbit.task.review_recover", payload.clone())
        .unwrap();
    assert_eq!(replay["replayed"], true);
    assert_eq!(first["admission"], replay["admission"]);
    let mut changed = payload.clone();
    changed["reason"] = json!("Different approval is not the same payload.");
    assert!(
        owner
            .run_tool_as_human("orbit.task.review_recover", changed)
            .is_err()
    );
    assert_eq!(
        f.leaf.owner_artifact(REVIEW_GATE_ARTIFACT).unwrap(),
        rejected
    );
    let claim_after = f.leaf.pair.owner_claim_for_leaf(&f.leaf.leaf);
    assert_eq!(claim_after["settlement"], claim_before["settlement"]);
    assert_eq!(
        claim_after["preserved_candidate"],
        claim_before["preserved_candidate"]
    );
    assert!(
        owner
            .submit_job_run(
                "task_pr_pipeline",
                json!({"task_ids":[f.leaf.task]}),
                Some("operator")
            )
            .is_err(),
        "ordinary owner delivery cannot consume a claim-only recovery admission"
    );
    let mut legacy = f.leaf.pair.admission(&f.leaf.leaf).request;
    legacy.request_id = "legacy-without-held-recovery".into();
    legacy.held_recovery = false;
    let mut input = serde_json::to_value(legacy).unwrap();
    input["workspace"] = json!(owner.workspace_id().unwrap());
    let deferred = f
        .leaf
        .pair
        .wire
        .call("owner", "orbit.task.pull", input)
        .unwrap();
    assert!(
        deferred["receipt"]["claim"].is_null(),
        "an executor that cannot authenticate recovery context must not consume the admission"
    );
    assert!(
        !deferred["receipt"]["deferred_conflicts"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let next = f.leaf.pair.running_leaf(&f.leaf.drain, 1);
    let receipt = f.leaf.pair.admission(&next).receipt.unwrap();
    let candidate = receipt
        .task
        .unwrap()
        .resume_candidate
        .expect("explicit amendment retained source");
    assert_eq!(candidate.branch, f.branch);
    assert_eq!(candidate.pull_request.as_deref(), Some("41"));
    assert_eq!(
        candidate.held_recovery.as_ref().unwrap().binding,
        serde_json::from_value(inspected["binding"].clone()).unwrap()
    );
    let history = owner.get_task_history(&f.leaf.task).unwrap();
    assert_eq!(
        history
            .iter()
            .filter(|e| e.event == HELD_RECOVERY_ADMITTED)
            .count(),
        1
    );
    assert_eq!(
        history
            .iter()
            .filter(|e| e.event == HELD_RECOVERY_CLAIMED)
            .count(),
        1
    );
    let leaf_admission = f.leaf.pair.admission(&next);
    let claim = leaf_admission
        .receipt
        .as_ref()
        .unwrap()
        .claim
        .as_ref()
        .unwrap();
    let bound = f
        .leaf
        .pair
        .follower
        .clone()
        .with_worker_invocation(
            WorkerInvocation {
                owner_machine_id: OWNER.into(),
                owner_workspace_id: leaf_admission.destination.owner_workspace_id.clone(),
                owner_destination: leaf_admission.destination.selector.clone(),
                task_id: f.leaf.task.clone(),
                claim_id: claim.claim_id.clone(),
                execution: claim.executed_on.clone(),
                bound_run_id: next.clone(),
            },
            Arc::new(ToOwner(owner.clone())),
        )
        .unwrap();
    let setup = super::candidate_carry::engine_action(
        &bound,
        "worktree_setup",
        &json!({"job_run_id":next,"run_id":next,"task_ids":[f.leaf.task],"base":"main","base_sync":"remote","dependency_delivery":"ignore","claimed":true}),
    );
    let resumed = super::candidate_carry::engine_action(
        &bound,
        "candidate_resume",
        &json!({"job_run_id":next,"task_ids":[f.leaf.task],"workspace_path":setup["workspace_path"],"base_sha":setup["base_sha"],"candidate":candidate,"claimed":true}),
    );
    let mut checkpoint = f.leaf.pair.follower.read_run_state(&next).unwrap().unwrap();
    checkpoint.pipeline["resume_candidate"] = resumed.clone();
    checkpoint.step_outputs.insert(1, resumed.clone());
    checkpoint.step_states.insert(1, JobRunState::Success);
    f.leaf
        .pair
        .follower
        .write_run_state(&next, &checkpoint)
        .unwrap();
    assert_eq!(
        resumed["implement"], true,
        "old host checks never skip the actual admitted repair"
    );
    assert_eq!(resumed["reused_branch"], f.branch);
    assert_eq!(
        resumed["repair"]["held_recovery"]["binding"]["delta"],
        inspected["binding"]["delta"]
    );
    assert_eq!(
        owner
            .run_tool_as_human("orbit.task.review_recover", payload)
            .unwrap()["replayed"],
        true,
        "lost replies reconcile after task ownership changes"
    );
    let checkout = PathBuf::from(setup["workspace_path"].as_str().unwrap());
    f.repair_source(&checkout);
    let implementation = json!({"execution_summary":"Repaired the real assertion under the approved contract; preserved incomplete-evidence negative controls."});
    super::candidate_carry::engine_action(
        &bound,
        "git_commit",
        &json!({"job_run_id":next,"scope":"all","workspace_path":checkout,"base_sha":setup["base_sha"],"implementation":implementation}),
    );
    let repaired = git(&checkout, &["rev-parse", "HEAD"]).trim().to_string();
    assert_ne!(
        repaired,
        inspected["binding"]["candidate_sha"].as_str().unwrap()
    );
    let validated = super::candidate_carry::engine_action(
        &bound,
        "claim_validate",
        &json!({"run_id":next,"job_run_id":next,"workspace_path":checkout,"base_sha":setup["base_sha"],"base_sync":"remote","implementation":implementation}),
    );
    assert_eq!(
        validated["tested_head"], repaired,
        "native gates run on the actual repaired head"
    );
    assert_eq!(validated["results"][0]["exit_code"], 0);
    assert!(
        validated["results"][0]["output"]
            .as_str()
            .unwrap()
            .contains("cleanup assertion passed"),
        "the repaired cleanup guarantee must execute under fresh host validation"
    );
    let mut state = f.leaf.pair.follower.read_run_state(&next).unwrap().unwrap();
    state.pipeline["validate"] = validated;
    f.leaf.pair.follower.write_run_state(&next, &state).unwrap();
    let input = json!({"run_id":next,"job_run_id":next,"completed_task_ids":[f.leaf.task],"workspace_path":checkout,"base_sha":setup["base_sha"],"base":"main","base_sync":"remote","mode":"pr","before_landing":true,"host_validation_step":"validate"});
    let admitted = bound
        .run_deterministic(
            "review_gate_admit",
            &json!({}),
            &input,
            ToolContext::default(),
        )
        .unwrap();
    assert_ne!(
        admitted["attempt_id"], inspected["binding"]["attempt_id"],
        "repaired source receives a fresh independent attempt"
    );
    assert_eq!(admitted["head_sha"], repaired);
    let mut reviewed = report(
        &admitted,
        ReviewVerdict::Accept,
        vec![],
        vec![record("V1", fixture::ASSERTION, ValidationOutcome::Passed)],
        None,
    );
    reviewed.summary =
        "Independently reviewed the approved fixture amendment and its retained negative controls."
            .into();
    let source = checkout.join(".orbit/tmp/repaired-review.json");
    std::fs::create_dir_all(source.parent().unwrap()).unwrap();
    std::fs::write(&source, serde_json::to_vec(&reviewed).unwrap()).unwrap();
    bound.run_tool("orbit.task.artifact.put",json!({"id":f.leaf.task,"model":"codex","path":"review-report.json","source_path":source})).unwrap();
    let mut settle = input;
    settle["admission"] = admitted;
    let accepted = bound
        .run_deterministic(
            "review_gate_settle",
            &json!({}),
            &settle,
            ToolContext::default(),
        )
        .unwrap();
    assert_eq!(accepted["verdict"], "accept");
    let certificate: ReviewCertificate = serde_json::from_slice(
        &owner
            .get_task_artifact(&f.leaf.task, REVIEW_GATE_ARTIFACT)
            .unwrap()
            .unwrap()
            .content,
    )
    .unwrap();
    assert_eq!(certificate.final_candidate.commit, repaired);
    assert_eq!(
        certificate.resumed_hold_attempt, None,
        "no original rejected coverage carries to repaired source"
    );
    let push = json!({"run_id":next,"job_run_id":next,"completed_task_ids":[f.leaf.task],"workspace_path":checkout,"branch":f.branch,"candidate_resume":resumed,"implementation":implementation});
    std::fs::write(checkout.join("outside-scope.txt"), "unapproved change\n").unwrap();
    git(&checkout, &["add", "outside-scope.txt"]);
    git(&checkout, &["commit", "-qm", "unapproved source change"]);
    let refused = orbit_engine::execute_deterministic_action(
        &bound,
        "git_push",
        &json!({}),
        &push,
        false,
        &Default::default(),
        None,
    )
    .expect_err("approved scope must fence the actual publication");
    assert!(
        refused.to_string().contains("approved selectors"),
        "{refused}"
    );
    git(&checkout, &["branch", "unpublished-outside-scope", "HEAD"]);
    git(&checkout, &["reset", "--hard", &repaired]);
    let pushed = super::candidate_carry::engine_action(&bound, "git_push", &push);
    assert_eq!(pushed["branch"], f.branch);
    assert_eq!(pushed["local_sha"], repaired);
    assert!(
        git(
            &checkout,
            &["ls-remote", "origin", &format!("refs/heads/{}", f.branch)]
        )
        .starts_with(&repaired),
        "the new head replaces the original PR branch under its exact lease"
    );
}

#[test]
fn evidence_adjudication_preserves_the_incomplete_hold_and_refuses_stale_leases() {
    if !isolated(
        module_path!(),
        "evidence_adjudication_preserves_the_incomplete_hold_and_refuses_stale_leases",
    ) {
        return;
    }
    let f = Fixture::new(true);
    f.advance_base();
    let owner = &f.leaf.pair.wire.owner;
    let hold = f
        .leaf
        .owner_artifact(REVIEW_EVIDENCE_HOLD_ARTIFACT)
        .unwrap();
    let inspected = f.inspect("adjudicate_evidence");
    let payload = f.submit(&inspected["binding"]);
    assert!(
        f.leaf
            .bound
            .run_tool("orbit.task.review_recover", payload.clone())
            .is_err(),
        "agent authority cannot admit its own held review"
    );
    let original_answer = std::fs::read(&f.answer).unwrap();
    let mut moved: Value = serde_json::from_slice(&original_answer).unwrap();
    moved["autoMergeRequest"] = json!({"enabledAt":"2026-10-15T00:00:00Z"});
    std::fs::write(&f.answer, moved.to_string()).unwrap();
    assert!(
        owner
            .run_tool_as_human("orbit.task.review_recover", payload.clone())
            .is_err(),
        "uncertain merge intent grants no admission"
    );
    assert_eq!(
        owner
            .get_task_history(&f.leaf.task)
            .unwrap()
            .iter()
            .filter(|e| e.event == HELD_RECOVERY_ADMITTED)
            .count(),
        0
    );
    std::fs::write(&f.answer, original_answer).unwrap();
    let admitted = owner
        .run_tool_as_human("orbit.task.review_recover", payload)
        .unwrap();
    assert_eq!(
        admitted["admission"]["binding"]["candidate_sha"],
        inspected["binding"]["candidate_sha"]
    );
    assert_eq!(
        f.leaf
            .owner_artifact(REVIEW_EVIDENCE_HOLD_ARTIFACT)
            .unwrap(),
        hold,
        "admission cannot synthesize receipt or rewrite the original requirement"
    );
    assert!(
        !owner
            .get_task_history(&f.leaf.task)
            .unwrap()
            .iter()
            .any(|e| e.event == "review_evidence_received")
    );
    let next = f.leaf.pair.running_leaf(&f.leaf.drain, 1);
    let leaf_admission = f.leaf.pair.admission(&next);
    let claim = leaf_admission
        .receipt
        .as_ref()
        .unwrap()
        .claim
        .as_ref()
        .unwrap();
    let bound = f
        .leaf
        .pair
        .follower
        .clone()
        .with_worker_invocation(
            WorkerInvocation {
                owner_machine_id: OWNER.into(),
                owner_workspace_id: leaf_admission.destination.owner_workspace_id.clone(),
                owner_destination: leaf_admission.destination.selector.clone(),
                task_id: f.leaf.task.clone(),
                claim_id: claim.claim_id.clone(),
                execution: claim.executed_on.clone(),
                bound_run_id: next.clone(),
            },
            Arc::new(ToOwner(owner.clone())),
        )
        .unwrap();
    let setup = bound
        .run_deterministic(
            "review_adjudication_prepare",
            &json!({}),
            &json!({"run_id":next}),
            ToolContext::default(),
        )
        .unwrap();
    let observed = bound
        .run_deterministic(
            "review_adjudication_prepare",
            &json!({}),
            &json!({"run_id":next,"observe":true}),
            ToolContext::default(),
        )
        .unwrap();
    assert_eq!(observed["pr_number"], "41");
    assert_ne!(
        setup["source_base_sha"], setup["target_base_sha"],
        "adjudication remains usable after integration advances"
    );
    let checkout = PathBuf::from(setup["workspace_path"].as_str().unwrap());
    assert_eq!(
        git(&checkout, &["rev-parse", "HEAD"]).trim(),
        inspected["binding"]["candidate_sha"].as_str().unwrap()
    );
    let gate = json!({"run_id":next,"job_run_id":next,"completed_task_ids":[f.leaf.task],"workspace_path":checkout,"base_sha":setup["base_sha"],"base":"main","base_sync":"remote","mode":"pr","before_landing":true});
    let reviewed = bound
        .run_deterministic(
            "review_gate_admit",
            &json!({}),
            &gate,
            ToolContext::default(),
        )
        .unwrap();
    assert_ne!(reviewed["attempt_id"], inspected["binding"]["attempt_id"]);
    assert_ne!(
        reviewed["decision"], "evidence_received",
        "the incomplete certificate cannot substitute for fresh adjudication"
    );
    let replacement = ReviewEvidenceRequirement {
        kind: ReviewEvidenceKind::NativeOs,
        name: "Real confined assertion and incomplete negative controls".into(),
        command: "printf 'replacement proof executed\\n'".into(),
        artifact: "evidence/replacement.json".into(),
        os: None,
        hosted_check: None,
    };
    let mut reviewer = report(
        &reviewed,
        ReviewVerdict::Incomplete,
        vec![],
        vec![
            record("V1", "true", ValidationOutcome::Passed),
            record("V2", &replacement.command, ValidationOutcome::NotRun),
        ],
        Some("The replacement must actually execute before ordinary completion."),
    );
    reviewer.external_evidence = vec![replacement.clone()];
    reviewer.adjudication = Some(orbit_types::workflow::HeldEvidenceDecision::Replace {reason:"A real confined assertion retains the cleanup guarantee and the negative incomplete-evidence controls without the impossible outer ps permission.".into(),requirements:vec![replacement.clone()]});
    let source = checkout.join(".orbit/tmp/adjudication-report.json");
    std::fs::create_dir_all(source.parent().unwrap()).unwrap();
    std::fs::write(&source, serde_json::to_vec(&reviewer).unwrap()).unwrap();
    bound.run_tool("orbit.task.artifact.put",json!({"id":f.leaf.task,"model":"codex","path":"review-report.json","source_path":source})).unwrap();
    let mut settle = gate;
    settle["admission"] = reviewed;
    let held = bound
        .run_deterministic(
            "review_gate_settle",
            &json!({}),
            &settle,
            ToolContext::default(),
        )
        .expect_err("prospective replacement remains incomplete");
    assert!(
        held.to_string().contains("review_awaiting_evidence"),
        "{held}"
    );
    let replaced: ReviewEvidenceHold = serde_json::from_slice(
        &owner
            .get_task_artifact(&f.leaf.task, REVIEW_EVIDENCE_HOLD_ARTIFACT)
            .unwrap()
            .unwrap()
            .content,
    )
    .unwrap();
    assert_eq!(replaced.requirements, vec![replacement.clone()]);
    assert_eq!(
        replaced.candidate.commit,
        inspected["binding"]["candidate_sha"].as_str().unwrap()
    );
    assert_eq!(
        admitted["admission"]["evidence"]["hold"],
        serde_json::from_slice::<Value>(&hold).unwrap(),
        "the original hold stays immutable in the audit record"
    );
    let worker = ClaimInvocation::trusted_worker(
        f.leaf.task.clone(),
        claim.claim_id.clone(),
        FOLLOWER.into(),
        Some(ClaimRun {
            machine_id: FOLLOWER.into(),
            run_id: next.clone(),
        }),
    );
    owner
        .mutate_execution_claim(
            Some(&worker),
            "prospective-hold",
            &ClaimMutation::Release(ClaimEvidence {
                summary: Some("prospective replacement proof remains incomplete".into()),
                evidence_hold: Some(replaced.clone()),
                held_pull_request: Some(orbit_store::contracts::ClaimPullRequest {
                    number: "41".into(),
                    head: f.branch.clone(),
                }),
                ..Default::default()
            }),
        )
        .unwrap();
    let command = std::process::Command::new("sh")
        .args(["-c", &replacement.command])
        .output()
        .unwrap();
    assert!(command.status.success());
    let log = json!({"output":String::from_utf8(command.stdout).unwrap(),"exit_code":0});
    let evidence = |outcome| json!({"schema_version":1,"attempt_id":replaced.attempt_id,"candidate":replaced.candidate,"kind":"native_os","name":replacement.name,"command":replacement.command,"outcome":outcome,"log_artifact":"evidence/replacement-log.json"});
    let attach = |path: &str, content: Value| {
        owner
            .update_task_as_human(
                &f.leaf.task,
                orbit_core::application::task::TaskUpdateParams {
                    upsert_artifacts: vec![TaskArtifact {
                        path: path.into(),
                        content: content.to_string().into_bytes(),
                        media_type: "application/json".into(),
                        created_by: None,
                    }],
                    ..Default::default()
                },
                "human:fixture".into(),
            )
            .unwrap();
    };
    attach("evidence/replacement-log.json", log);
    attach("evidence/replacement.json", evidence("denied"));
    assert_eq!(
        f.leaf.pair.owner_status(&f.leaf.task),
        "in-progress",
        "denied replacement evidence cannot release the hold"
    );
    assert!(
        !owner
            .get_task_history(&f.leaf.task)
            .unwrap()
            .iter()
            .any(|e| e.event == "review_evidence_received")
    );
    attach("evidence/replacement.json", evidence("passed"));
    assert_eq!(
        f.leaf.pair.owner_status(&f.leaf.task),
        "backlog",
        "genuine passing replacement evidence queues ordinary completion review"
    );
}

#[test]
fn concurrent_and_restarted_submissions_keep_one_admission_and_refuse_changed_bindings() {
    if !isolated(
        module_path!(),
        "concurrent_and_restarted_submissions_keep_one_admission_and_refuse_changed_bindings",
    ) {
        return;
    }
    let f = Fixture::new(false);
    f.amendment();
    let owner = &f.leaf.pair.wire.owner;
    let binding = f.inspect("source_repair")["binding"].clone();
    let payload = f.submit(&binding);
    for field in [
        "workspace_id",
        "machine_id",
        "claim_id",
        "candidate_sha",
        "base_sha",
        "certificate_digest",
        "diagnostics_digest",
    ] {
        let mut changed = payload.clone();
        changed["binding"][field] = json!("different-authority");
        assert!(
            owner
                .run_tool_as_human("orbit.task.review_recover", changed)
                .is_err(),
            "changed {field} must admit nothing"
        );
    }
    let jobs = super::candidate_carry::owner_jobs(&f.leaf.pair);
    let active = jobs
        .insert_job_run(
            "task_pr_pipeline",
            1,
            Utc::now(),
            Some(json!({"task_ids":[f.leaf.task]})),
            None,
        )
        .unwrap();
    assert!(
        owner
            .run_tool_as_human("orbit.task.review_recover", payload.clone())
            .is_err(),
        "active delivery cannot be displaced"
    );
    jobs.finalize_job_run(&active.run_id, JobRunState::Cancelled, Utc::now(), None)
        .unwrap();
    assert_eq!(
        owner
            .get_task_history(&f.leaf.task)
            .unwrap()
            .iter()
            .filter(|e| e.event == HELD_RECOVERY_ADMITTED)
            .count(),
        0
    );
    let repo = &f.leaf.pair.follower_repo;
    git(
        repo,
        &[
            "push",
            "-q",
            "--force",
            "origin",
            &format!(
                "{}:refs/heads/main",
                binding["candidate_sha"].as_str().unwrap()
            ),
        ],
    );
    assert!(
        owner
            .run_tool_as_human("orbit.task.review_recover", payload.clone())
            .is_err(),
        "a moved base lease invalidates the approval without effects"
    );
    git(
        repo,
        &[
            "push",
            "-q",
            "--force",
            "origin",
            &format!("{}:refs/heads/main", binding["base_sha"].as_str().unwrap()),
        ],
    );
    let mut missing_scope = payload.clone();
    missing_scope["binding"]["repair_selectors"] = json!([]);
    assert!(
        owner
            .run_tool_as_human("orbit.task.review_recover", missing_scope)
            .is_err()
    );
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let handles: Vec<_> = (0..2)
        .map(|_| {
            let barrier = barrier.clone();
            let owner = owner.clone();
            let payload = payload.clone();
            std::thread::spawn(move || {
                barrier.wait();
                owner
                    .run_tool_as_human("orbit.task.review_recover", payload)
                    .unwrap()
            })
        })
        .collect();
    barrier.wait();
    let replies: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(replies[0]["admission"], replies[1]["admission"]);
    assert_eq!(replies.iter().filter(|r| r["replayed"] == true).count(), 1);
    let reopened = OrbitRuntime::from_roots(
        &f.leaf.pair._root.path().join(OWNER).join("global"),
        &f.leaf.pair.owner_repo.join(".orbit"),
    )
    .unwrap()
    .with_automation_machine_identity(Some(OWNER.into()))
    .with_actor(f.identity.clone());
    let replay = reopened
        .run_tool_as_human("orbit.task.review_recover", payload)
        .unwrap();
    assert_eq!(
        replay["replayed"], true,
        "restart after a lost reply resolves the original durable admission"
    );
    assert_eq!(replay["admission"], replies[0]["admission"]);
    assert_eq!(
        owner
            .get_task_history(&f.leaf.task)
            .unwrap()
            .iter()
            .filter(|e| e.event == HELD_RECOVERY_ADMITTED)
            .count(),
        1
    );
}

#[test]
fn discarded_candidates_cannot_gain_recovery_authority() {
    if !isolated(
        module_path!(),
        "discarded_candidates_cannot_gain_recovery_authority",
    ) {
        return;
    }
    let f = Fixture::new(false);
    f.amendment();
    let binding = f.inspect("source_repair")["binding"].clone();
    f.leaf
        .pair
        .wire
        .owner
        .update_task_as_human(
            &f.leaf.task,
            orbit_core::application::task::TaskUpdateParams {
                discard_candidate: true,
                ..Default::default()
            },
            "human:fixture".into(),
        )
        .unwrap();
    assert!(
        f.leaf
            .pair
            .wire
            .owner
            .run_tool_as_human("orbit.task.review_recover", f.submit(&binding))
            .is_err()
    );
    assert_eq!(
        f.leaf
            .pair
            .wire
            .owner
            .get_task_history(&f.leaf.task)
            .unwrap()
            .iter()
            .filter(|e| e.event == HELD_RECOVERY_ADMITTED)
            .count(),
        0
    );
}
