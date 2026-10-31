//! Real clean-base claim execution and owner settlement, without a provider PR.
//!
//! Scenario bodies only; the shared clean-leaf fixture, stand-in commands
//! and handoff helpers live in `support` (ORB-17794).
use super::claimed_review::ToOwner;
use super::*;

mod support;
mod task_artifacts;

pub(crate) use support::*;

fn verified_no_diff_and_already_satisfied_base_complete_without_a_pr() {
    for (already_landed, job) in [
        (false, "task_claimed_pr_pipeline"),
        (true, "task_claimed_pr_pipeline"),
        (false, "task_claimed_local_pipeline"),
    ] {
        let unrelated = if job == "task_claimed_pr_pipeline" {
            CleanLeaf::large_unrelated_evidence()
        } else {
            Vec::new()
        };
        let fixture = CleanLeaf::with_evidence(already_landed, "done", &unrelated);
        if !unrelated.is_empty() {
            fixture.assert_bulk_evidence_exceeds_ceiling();
        }
        let handoff = fixture.run_clean_pipeline(job);
        fixture.assert_named_reads_and_retained_evidence(&unrelated);
        fixture.settle_review(&handoff);
        let landed = fixture.land(&handoff);
        assert_eq!(landed["evidence"]["external_merge"], false, "{landed}");
        fixture.assert_status("done");
        assert_eq!(
            git(&fixture.pair.owner_repo, &["rev-parse", "main"]).trim(),
            fixture.base
        );
        fixture.assert_no_blocked();
        let replay = fixture.settle(&handoff).unwrap();
        assert_eq!(
            replay["status"], "review",
            "journal replays acceptance without repeating completion"
        );
        fixture.assert_status("done");
    }
}

/// The full-code-review chore shape: a `no-diff-expected` task the follower
/// claims, whose leaf files findings on the owner through the claimed-owner
/// broker and hands off `NoDiff`, with no PR [ORB-14474].
fn a_follower_claims_no_diff_expected_work_files_findings_and_completes_without_a_pr() {
    let fixture = CleanLeaf::tagged("done");
    let owner_task = fixture.pair.owner_task(&fixture.task);
    assert!(
        owner_task["tags"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tag| tag == orbit_types::task::NO_DIFF_EXPECTED_TAG),
        "{owner_task:#}"
    );

    let filed = fixture
        .bound
        .run_tool(
            "orbit.task.add",
            json!({
                "title": "Finding from the review chore",
                "description": "Found while reviewing the crate.",
                "complexity": "low", "model": "codex",
                "relations": [{"type": "spawned_from", "target": fixture.task}],
            }),
        )
        .expect("a claimed no-diff leaf files a finding on the owner");
    let id = filed["id"].as_str().expect("the new task's id");
    let prefix = |id: &str| id.split('-').next().unwrap().to_string();
    assert_eq!(prefix(id), prefix(&fixture.task), "owner prefix: {filed}");
    let stored = fixture
        .pair
        .wire
        .owner
        .run_tool("orbit.task.show", json!({"id": id}))
        .expect("the owner holds the filed task");
    assert_eq!(stored["id"], id, "{stored}");

    let handoff = fixture.run_clean_pipeline("task_claimed_pr_pipeline");
    fixture
        .settle(&handoff)
        .expect("owner independently verifies the report");
    fixture.land(&handoff);
    fixture.assert_status("done");
    fixture.assert_no_blocked();
}

/// [ORB-15962] A claimed review's coverage, put through the worker's own
/// artifact route, is stamped by the owner with the claim, follower and bound
/// run it authenticated. After the no-diff landing settles that claim and its
/// mutation receipts are gone, the owner still accepts exactly those bytes
/// for the frozen batch naming the task, though its store has no record of
/// the follower's run. A witness naming any other claim, machine or run, a
/// late put from the landed claim, and bytes the witness does not cover are
/// all refused. Before, the claimed route stored no witness, so the batch
/// stayed owed as `unauthorized_submitter`.
#[test]
fn claimed_coverage_is_authenticated_by_the_owners_claim_journal_after_landing() {
    use orbit_types::workflow::automation::{
        COVERAGE_ARTIFACT, ClaimedSubmission, EVIDENCE_AUTHORITY_ARTIFACT, EvidenceSubmission,
        ExaminationCheck, evidence_template,
    };

    if !isolated(
        module_path!(),
        "claimed_coverage_is_authenticated_by_the_owners_claim_journal_after_landing",
    ) {
        return;
    }
    let fixture = CleanLeaf::review_only("done");
    let owner = &fixture.pair.wire.owner;
    let attempt = reviewed_batch(&fixture);
    let mut coverage = evidence_template(&attempt);
    coverage.examination_complete = true;
    coverage.checks = vec![ExaminationCheck {
        subject: "src/reviewed.rs".into(),
        method: "read the delivery diff".into(),
        observation: "adds one empty function".into(),
    }];
    coverage.delivery_examinations[0].examined_paths = vec!["src/reviewed.rs".into()];
    coverage.delivery_examinations[0].rationale =
        "The delivery adds an empty function that nothing calls yet.".into();
    fixture.attach(COVERAGE_ARTIFACT, serde_json::to_value(&coverage).unwrap());
    let refused = fixture
        .bound
        .attach_claim_validation_log(EVIDENCE_AUTHORITY_ARTIFACT, b"{}".to_vec())
        .expect_err("a worker cannot write the owner's witness");
    assert!(refused.to_string().contains("reserved"), "{refused}");

    let handoff = fixture.run_clean_pipeline("task_claimed_pr_pipeline");
    fixture.settle(&handoff).expect("the owner accepts it");
    fixture.land(&handoff);
    fixture.assert_status("done");
    admit(owner, &attempt);

    let bytes = owner
        .get_task_artifact(&fixture.task, COVERAGE_ARTIFACT)
        .unwrap()
        .unwrap()
        .content;
    let witness_bytes = owner
        .get_task_artifact(&fixture.task, EVIDENCE_AUTHORITY_ARTIFACT)
        .unwrap()
        .expect("the owner stamps the claimed submission")
        .content;
    let witness: EvidenceSubmission = serde_json::from_slice(&witness_bytes).unwrap();
    assert_eq!(
        witness,
        EvidenceSubmission {
            action_id: fixture.task.clone(),
            evidence_digest: sha256_hex(&bytes),
            run_id: fixture.leaf.clone(),
            claim: Some(ClaimedSubmission {
                claim_id: handoff.claim_id.clone(),
                machine_id: FOLLOWER.into(),
            }),
        }
    );

    // Settlement prunes a landed claim's receipts after a week; the proof
    // must not need them, nor the follower's run, which this store lacks.
    let workspace = owner.workspace_id().unwrap();
    let pruned = owner
        .sqlite_store()
        .unwrap()
        .with_transaction(|tx| {
            Ok(tx
                .connection()
                .execute(
                    "DELETE FROM task_coordination_rows WHERE workspace_id = ?1 AND kind = ?2",
                    [workspace.as_str(), "distributed-claim-mutation-v1"],
                )
                .unwrap())
        })
        .unwrap();
    assert!(pruned > 0, "the claim's mutation receipts existed");
    assert!(owner.show_job_run(&fixture.leaf).is_err());
    let accepted_coverage = || RuntimeHost::accepted_automation_coverage(owner, &fixture.task);
    assert_eq!(accepted_coverage().unwrap(), Some(coverage.clone()));

    let witness_path = find_file(&owner.paths().global_dir, EVIDENCE_AUTHORITY_ARTIFACT)
        .or_else(|| find_file(&owner.paths().orbit_dir, EVIDENCE_AUTHORITY_ARTIFACT))
        .expect("the witness file");
    let foreign = |edit: fn(&mut EvidenceSubmission)| {
        let mut forged = witness.clone();
        edit(&mut forged);
        forged
    };
    for (case, forged) in [
        (
            "foreign claim",
            foreign(|w| w.claim.as_mut().unwrap().claim_id = "claim-elsewhere".into()),
        ),
        (
            "foreign machine",
            foreign(|w| w.claim.as_mut().unwrap().machine_id = OWNER.into()),
        ),
        ("stale run", foreign(|w| w.run_id = "jrun-earlier".into())),
        ("unclaimed follower run", foreign(|w| w.claim = None)),
    ] {
        std::fs::write(&witness_path, serde_json::to_vec(&forged).unwrap()).unwrap();
        assert_eq!(accepted_coverage().unwrap(), None, "{case}");
    }
    std::fs::write(&witness_path, &witness_bytes).unwrap();
    assert_eq!(accepted_coverage().unwrap(), Some(coverage.clone()));

    // The landed claim takes no further put, and bytes the owner writes
    // itself carry no claimed witness.
    let mut replaced = coverage.clone();
    replaced.findings = vec!["ORB-1".into()];
    let late = fixture
        .bound
        .attach_claim_validation_log(COVERAGE_ARTIFACT, serde_json::to_vec(&replaced).unwrap())
        .expect_err("a landed claim refuses its worker's put");
    assert!(late.to_string().contains("stale_claim"), "{late}");
    owner
        .update_task_as_human(
            &fixture.task,
            orbit_core::application::task::TaskUpdateParams {
                upsert_artifacts: vec![TaskArtifact {
                    path: COVERAGE_ARTIFACT.into(),
                    media_type: "application/json".into(),
                    content: serde_json::to_vec(&replaced).unwrap(),
                    created_by: None,
                }],
                ..Default::default()
            },
            "fixture operator".into(),
        )
        .unwrap();
    assert_eq!(accepted_coverage().unwrap(), None, "tampered bytes");
}

/// [ORB-15764] A claimed `no-diff-expected` review compiles nothing, as the
/// owner's own pipelines skip `validate` for the tag: its leaf runs no
/// required command and replays no deferred gate on its clean base, and hands
/// off `NoDiff` with no validation logs. The owner accepts, approves and lands
/// that handoff without running them either. The required command here fails
/// whenever it runs, and both it and the gate count every run.
#[test]
fn a_claimed_no_diff_expected_skip_runs_no_validation_and_the_owner_lands_it() {
    use orbit_core::application::review::ExpectedCandidate;

    if !isolated(
        module_path!(),
        "a_claimed_no_diff_expected_skip_runs_no_validation_and_the_owner_lands_it",
    ) {
        return;
    }
    for (job, step, completion) in [
        ("task_claimed_pr_pipeline", "validate_no_diff", "done"),
        ("task_claimed_local_pipeline", "validate", "done"),
        ("task_claimed_pr_pipeline", "validate_no_diff", "review"),
    ] {
        let check = RedCheck::new();
        let unrelated = if completion == "done" && job == "task_claimed_pr_pipeline" {
            CleanLeaf::large_unrelated_evidence()
        } else {
            Vec::new()
        };
        let mut fixture = CleanLeaf::tagged_requiring(completion, &check, &unrelated);
        if !unrelated.is_empty() {
            fixture.assert_bulk_evidence_exceeds_ceiling();
        }
        fixture.hand_off_deferral();
        fixture.attach_coverage();
        let outcome = fixture.execute_clean_pipeline(job).unwrap();
        assert!(outcome.success, "{job}: {outcome:#?}");
        // The same matrix also pins the original ORB-14791 clean-review route.
        fixture.assert_named_reads_and_retained_evidence(&unrelated);
        let committed = &outcome.pipeline["commit"];
        assert_eq!(committed["decision"], "skipped_no_diff_expected", "{job}");
        assert_eq!(committed["base_sha"], fixture.base, "{job}");
        for skipped in ["prepare_branch", "sync_base", "review", "push", "pr_open"] {
            assert!(
                outcome.pipeline.get(skipped).is_none(),
                "{job}: {outcome:#?}"
            );
        }
        let validated = &outcome.pipeline[step];
        assert_eq!(
            validated["decision"], "skipped_no_diff_expected",
            "{job}: {validated:#}"
        );
        assert_eq!(validated["validation"], json!([]), "{job}");
        let ClaimMutation::AcceptHandoff(handoff) =
            fixture.pair.admission(&fixture.leaf).settlement.unwrap()
        else {
            panic!("{job}: typed handoff required")
        };
        assert!(
            matches!(handoff.candidate.delivery, HandoffDelivery::NoDiff { .. }),
            "{job}"
        );
        assert!(handoff.validation.is_empty(), "{job}: {handoff:?}");
        assert!(
            handoff
                .execution_summary
                .contains("no-diff-expected and changed nothing"),
            "{job}: {}",
            handoff.execution_summary
        );

        let settled = fixture
            .settle(&handoff)
            .expect("the owner accepts a tagged skip without logs");
        assert_eq!(settled["status"], "review", "{job}: {settled}");
        let owner = &fixture.pair.wire.owner;
        assert_eq!(fixture.pair.owner_status(&fixture.task), "review", "{job}");
        let artifacts = fixture
            .owner_artifacts()
            .into_iter()
            .map(|artifact| artifact.path)
            .collect::<Vec<_>>();
        assert!(
            artifacts
                .iter()
                .any(|path| path == "automation-coverage.json"),
            "{job}: coverage reaches the owner: {artifacts:?}"
        );
        assert!(
            !artifacts
                .iter()
                .any(|path| path == "already-landed.json" || path == "no-diff.json"),
            "{job}: a review writes no clean-tree report: {artifacts:?}"
        );
        let accepted = fixture.accepted(&handoff);
        if completion == "review" {
            assert!(owner.landing_start_requests().unwrap().is_empty(), "{job}");
            owner
                .approve_handoff_as_operator(
                    &accepted.handoff_id,
                    &ExpectedCandidate {
                        candidate_commit: fixture.base.clone(),
                        base_commit: fixture.base.clone(),
                    },
                    "operator",
                    "approve-no-diff-expected",
                )
                .expect("the operator approves a tagged skip without logs");
        }
        let landed = fixture.land(&handoff);
        assert_eq!(landed["evidence"]["delivery"], "no_diff", "{landed}");
        assert_eq!(
            landed["evidence"]["external_merge"], false,
            "{job}: {landed}"
        );
        assert_eq!(fixture.pair.owner_status(&fixture.task), "done", "{job}");
        assert_eq!(check.runs(), 0, "{job}: no host ran the required command");
        assert_eq!(
            fixture.gate.as_ref().unwrap().runs(),
            0,
            "{job}: no host replayed the deferred gate"
        );
        fixture.assert_no_blocked();
    }
}

/// [ORB-15764] A claim admitted before the owner upgraded can settle after it.
/// Its older leaf still pinned a log per required command for the tagged
/// skip; the upgraded owner accepts that handoff, and still checks each log
/// it carries, so one pinned for another run is refused.
fn an_owner_accepts_a_tagged_skip_that_still_carries_logs_and_checks_them() {
    for forged in [false, true] {
        let check = RedCheck::new();
        let fixture = CleanLeaf::tagged_requiring("done", &check, &[]);
        let mut handoff = fixture.prepare_handoff();
        assert!(handoff.validation.is_empty(), "{handoff:?}");
        let log = HandoffValidationLog {
            schema_version: 1,
            workspace_id: handoff.workspace_id.clone(),
            task_id: handoff.task_id.clone(),
            claim_id: handoff.claim_id.clone(),
            machine_id: handoff.machine_id.clone(),
            run_id: if forged {
                "another-run".into()
            } else {
                handoff.run_id.clone()
            },
            candidate: handoff.candidate.clone(),
            tested_head: handoff.candidate.candidate.commit.clone(),
            command: check.command(),
            exit_code: 0,
            output: "test suite::broken ... ok".into(),
            known_red: None,
            flaky: None,
        };
        let path = format!("validation/{}/0.json", handoff.claim_id);
        let content = serde_json::to_vec(&log).unwrap();
        fixture
            .bound
            .attach_claim_validation_log(&path, content.clone())
            .unwrap();
        handoff.validation.push(HandoffArtifactRef {
            path,
            sha256: sha256_hex(&content),
        });
        let settled = fixture.settle(&handoff);
        if forged {
            let error = settled.expect_err("a log for another run is refused");
            assert!(
                error
                    .to_string()
                    .contains("validation must pass for the exact claim/run/candidate/base"),
                "{error}"
            );
            fixture.assert_status("in-progress");
        } else {
            let settled = settled.expect("the owner accepts the older leaf's logs");
            assert_eq!(settled["status"], "review", "{settled}");
        }
        assert_eq!(check.runs(), 0, "forged={forged}");
        fixture.assert_no_blocked();
    }
}

/// A claimed tagged task hands off `NoDiff` and has no PR route for a change,
/// so a pending diff or a commit of its own is refused by name, before the
/// index is touched [ORB-14791].
fn a_claimed_no_diff_expected_task_that_changed_the_worktree_is_refused() {
    for committed in [false, true] {
        let fixture = CleanLeaf::review_only("done");
        let follower = &fixture.pair.follower_repo;
        std::fs::write(follower.join("src/f0.rs"), "fn reviewed() {}\n").unwrap();
        if committed {
            git(follower, &["commit", "-q", "-am", "Review edits code"]);
        }
        let error = engine_action(&fixture.bound, "git_commit", &fixture.input).unwrap_err();
        assert!(
            error.to_string().contains("no_diff_expected_changed"),
            "committed={committed}: {error}"
        );
        assert!(
            git(follower, &["diff", "--cached", "--name-only"]).is_empty(),
            "the refusal stages nothing"
        );
        if !committed {
            assert_eq!(git(follower, &["rev-parse", "HEAD"]).trim(), fixture.base);
        }
        fixture.assert_status("in-progress");
        fixture.assert_no_blocked();
    }
}

/// The tag is the only authority a review's skip carries, and the owner's copy
/// of the task holds it: a skip checkpoint pinned for an untagged task is
/// refused at settlement, without blocking the task [ORB-14791].
fn owner_refuses_a_no_diff_expected_skip_for_an_untagged_task() {
    let fixture = CleanLeaf::new(false, "done");
    let mut handoff = fixture.prepare_handoff();
    let skip = json!({
        "phase": "commit", "decision": "skipped_no_diff_expected", "committed": false,
        "skipped_no_diff_expected": true, "task_id": fixture.task,
        "job_run_id": fixture.leaf, "base_sha": fixture.base,
    });
    let path = "no-diff-handoff/forged-skip.json";
    fixture.attach(path, skip.clone());
    handoff.candidate.delivery = HandoffDelivery::NoDiff {
        evidence: HandoffArtifactRef {
            path: path.into(),
            sha256: sha256_hex(&serde_json::to_vec(&skip).unwrap()),
        },
    };
    let error = fixture.settle(&handoff).unwrap_err();
    assert!(
        error.to_string().contains("no-diff-expected"),
        "the refusal names the missing tag: {error}"
    );
    fixture.assert_status("in-progress");
    assert!(
        fixture
            .pair
            .wire
            .owner
            .accepted_task_handoff(&handoff.claim_id)
            .is_err()
    );
    fixture.assert_no_blocked();
}

/// A follower's claimed `delivery-code-review` files each finding on the
/// owner with `regression_from` the task that introduced it, which the owner
/// checks is one of its own tasks, and the owner records the claim and run
/// that filed it. A finding the owner still refuses reaches it as the review
/// task's `unfiled-findings.json`, which the handoff attaches [ORB-14792].
fn a_claimed_delivery_review_files_regression_findings_and_hands_off_the_rest() {
    let mut fixture = CleanLeaf::delivery_review("done");
    let owner = fixture.pair.wire.owner.clone();
    let culprit = owner
        .run_tool(
            "orbit.task.add",
            json!({
                "title": "The delivery that introduced the defect",
                "description": "Landed in the reviewed batch.",
                "complexity": "low", "model": "codex",
                "workspace": fixture.pair.owner_repo.to_string_lossy(),
            }),
        )
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let relations = |culprit: &str| {
        json!([{"type": "spawned_from", "target": fixture.task},
               {"type": "regression_from", "target": culprit}])
    };
    let file = |culprit: &str| {
        fixture.bound.run_tool(
            "orbit.task.add",
            json!({
                "title": "Finding from the delivery review",
                "description": "The culprit delivery broke the invariant.",
                "complexity": "low", "type": "bug", "model": "codex",
                "tags": ["delivery-code-review"], "relations": relations(culprit),
            }),
        )
    };

    let filed = file(&culprit).expect("a claimed review files a regression finding");
    let id = filed["id"].as_str().expect("the finding's id");
    assert_eq!(
        fixture.pair.owner_task(id)["relations"],
        relations(&culprit)
    );
    let binding = fixture.bound.worker_invocation().unwrap().clone();
    assert!(
        owner
            .get_task_comments(id)
            .unwrap()
            .iter()
            .any(|comment| comment.message.contains(&binding.claim_id)
                && comment.message.contains(&fixture.leaf)),
        "the finding records the claim and run that filed it"
    );

    let before = owner.list_tasks().unwrap().len();
    let prefix = culprit.split('-').next().unwrap();
    let missing = file(&format!("{prefix}-999999")).unwrap_err().to_string();
    assert!(
        missing.contains("not a task in the claimed task's workspace"),
        "a culprit outside the owner's workspace is refused: {missing}"
    );
    assert_eq!(owner.list_tasks().unwrap().len(), before);

    let unfiled = json!([{
        "title": "A finding the owner refused",
        "description": "The owner refused it during the run.",
        "relations": relations(&culprit),
    }]);
    fixture.input["implementation"]["unfiled_findings"] = unfiled.clone();
    let handoff = fixture.run_clean_pipeline("task_claimed_pr_pipeline");
    let attached = owner
        .run_tool(
            "orbit.task.artifact.get",
            json!({"id": fixture.task, "path": "unfiled-findings.json"}),
        )
        .expect("the handoff attaches the unfiled findings to the review task");
    let record: Value = serde_json::from_str(attached["content"].as_str().unwrap()).unwrap();
    assert_eq!(record["findings"], unfiled, "{record}");
    assert_eq!(record["claim_id"], binding.claim_id);
    assert_eq!(record["run_id"], fixture.leaf);
    assert!(
        handoff.execution_summary.contains("unfiled-findings.json"),
        "the owner's summary points at the attachment: {}",
        handoff.execution_summary
    );
    fixture
        .settle(&handoff)
        .expect("the owner accepts the review's handoff");
}

/// A candidate that was published with plain-string `unfiled_findings` (the
/// implement step now refuses them before delivery) still hands off: each
/// string becomes a finding object that keeps its full text, the entry and
/// the owner's summary say it was normalized, and object entries pass through
/// untouched. A malformed entry that is not a string stays refused, and so
/// does a malformed field other than the findings [ORB-14927].
fn a_published_candidates_string_findings_are_normalized_at_handoff() {
    let mut fixture = CleanLeaf::delivery_review("done");
    let owner = fixture.pair.wire.owner.clone();
    let long = format!("{} and on", "A very long first sentence".repeat(8));
    let object = json!({
        "title": "Already structured",
        "description": "Kept as returned.",
        "relations": [{"type": "spawned_from", "target": fixture.task}],
    });
    fixture.input["implementation"]["unfiled_findings"] = json!([
        "The owner refused this. It needs a follow-up on the retry path.",
        object,
        long,
    ]);
    let handoff = fixture.run_clean_pipeline("task_claimed_pr_pipeline");
    let attached = owner
        .run_tool(
            "orbit.task.artifact.get",
            json!({"id": fixture.task, "path": "unfiled-findings.json"}),
        )
        .expect("the handoff attaches the normalized findings");
    let record: Value = serde_json::from_str(attached["content"].as_str().unwrap()).unwrap();
    let findings = record["findings"].as_array().unwrap();
    assert_eq!(findings.len(), 3, "{record}");
    assert_eq!(findings[0]["title"], "The owner refused this.");
    assert_eq!(
        findings[0]["description"],
        "The owner refused this. It needs a follow-up on the retry path.",
        "the full text survives"
    );
    assert_eq!(findings[0]["normalized_from"], "string");
    assert_eq!(findings[1], object, "an object entry is kept verbatim");
    assert_eq!(findings[2]["description"], long);
    assert!(findings[2]["title"].as_str().unwrap().len() <= 120);
    assert_eq!(record["normalized_string_entries"], 2);
    assert!(
        handoff.execution_summary.contains("normalized_from"),
        "the owner's summary records the repair: {}",
        handoff.execution_summary
    );
    fixture
        .settle(&handoff)
        .expect("the owner accepts the normalized handoff");

    for value in [json!([42]), json!(["   "]), json!("not an array")] {
        let mut fixture = CleanLeaf::delivery_review("done");
        fixture.input["implementation"]["unfiled_findings"] = value.clone();
        let handed_off = fixture
            .execute_clean_pipeline("task_claimed_pr_pipeline")
            .is_ok_and(|outcome| outcome.success);
        assert!(!handed_off, "{value} is still refused at handoff");
        fixture.assert_no_handoff();
    }
}

/// A claimed implementer that attributes its task's regression returns the
/// culprit as `regression_from`, since it cannot write the task's relations.
/// The shipped claimed pipeline's handoff carries it, and the owner records
/// the relation when it accepts the handoff, so no operator has to add it
/// before landing. A handoff naming a culprit the owner cannot resolve is
/// refused and records nothing [F2026-10-453].
fn a_claimed_implementers_regression_culprit_is_recorded_at_handoff() {
    let mut fixture = CleanLeaf::new(false, "review");
    let owner = fixture.pair.wire.owner.clone();
    let culprit = owner
        .run_tool(
            "orbit.task.add",
            json!({
                "title": "The delivery that broke CI",
                "description": "Landed the failing change.",
                "complexity": "low", "model": "codex",
                "workspace": fixture.pair.owner_repo.to_string_lossy(),
            }),
        )
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    fixture.input["implementation"]["regression_from"] = json!([culprit]);
    let handoff = fixture.run_clean_pipeline("task_claimed_pr_pipeline");
    assert_eq!(handoff.regression_from, vec![culprit.clone()]);
    assert!(
        handoff.execution_summary.contains(&culprit),
        "the summary names the attributed culprit: {}",
        handoff.execution_summary
    );

    let prefix = culprit.split('-').next().unwrap();
    let mut unknown = handoff.clone();
    unknown.regression_from = vec![format!("{prefix}-999999")];
    let refused = fixture.settle(&unknown).unwrap_err().to_string();
    assert!(
        refused.contains("regression_from attribution was refused"),
        "{refused}"
    );
    assert_eq!(
        fixture.pair.owner_task(&fixture.task)["relations"],
        json!([])
    );

    fixture
        .settle(&handoff)
        .expect("the owner accepts the attributed handoff");
    assert_eq!(
        fixture.pair.owner_task(&fixture.task)["relations"],
        json!([{"type": "regression_from", "target": culprit}]),
        "the owner records the culprit without an operator write"
    );
}

fn no_diff_retains_owner_completion_authority() {
    let fixture = CleanLeaf::new(false, "review");
    let handoff = fixture.prepare_handoff();
    fixture.settle(&handoff).unwrap();
    fixture.assert_status("review");
    assert!(
        fixture
            .pair
            .wire
            .owner
            .landing_start_requests()
            .unwrap()
            .is_empty()
    );
    fixture.assert_no_blocked();
}

fn owner_refuses_changed_no_diff_evidence_base_or_missing_required_checks_without_blocking() {
    for changed in [
        "report",
        "log",
        "rewritten_base",
        "required_check",
        "missing_log",
        "oversize_log",
        "wrong_log_run",
        "missing_checkpoint",
        "wrong_task",
        "wrong_run",
        "checkpoint_base",
        "removed_tag",
        "oversize_checkpoint",
        "truncated_checkpoint",
        "foreign_path",
    ] {
        let tagged = !matches!(
            changed,
            "report"
                | "log"
                | "rewritten_base"
                | "required_check"
                | "missing_log"
                | "oversize_log"
                | "wrong_log_run"
        );
        let fixture = if tagged {
            CleanLeaf::review_only("done")
        } else {
            CleanLeaf::new(false, "done")
        };
        let mut handoff = fixture.prepare_handoff();
        let mut restore_blob = None;
        match changed {
            "report" => {
                let HandoffDelivery::NoDiff { evidence } = &handoff.candidate.delivery else {
                    panic!("NoDiff")
                };
                fixture.attach(&evidence.path, json!({"decision": "verified_no_diff"}));
            }
            "log" => fixture.attach(
                "implementation-validation.json",
                json!({
                    "run_id": fixture.leaf, "tested_head": fixture.base, "command": CHECK,
                    "exit_code": 0, "output": "changed after checkpoint",
                }),
            ),
            // An advance is accepted [ORB-15074]; a base the owner's history
            // no longer contains is not.
            "rewritten_base" => {
                let repo = &fixture.pair.owner_repo;
                std::fs::write(repo.join("src/f0.rs"), "fn rewritten() {}\n").unwrap();
                git(
                    repo,
                    &["commit", "-q", "-a", "--amend", "-m", "Base rewritten"],
                );
                git(
                    repo,
                    &["push", "-q", "-f", "origin", "HEAD:refs/heads/main"],
                );
            }
            "missing_log" => {
                let artifact = fixture
                    .pair
                    .wire
                    .owner
                    .get_task_artifact(&fixture.task, "no-diff.json")
                    .unwrap()
                    .unwrap();
                let mut report: Value = serde_json::from_slice(&artifact.content).unwrap();
                report["validation"][0]["log_artifact"] = json!("missing-validation.json");
                fixture.attach("no-diff.json", report);
            }
            "oversize_log" => {
                fixture
                    .bound
                    .attach_claim_validation_log(
                        "implementation-validation.json",
                        vec![b' '; 1_048_577],
                    )
                    .unwrap();
                let error = fixture
                    .bound
                    .get_task_artifact(&fixture.task, "implementation-validation.json")
                    .unwrap_err();
                assert!(error.to_string().contains("inline read limit"), "{error}");
            }
            "wrong_log_run" => fixture.attach(
                "implementation-validation.json",
                json!({
                    "run_id": "another-run", "tested_head": fixture.base, "command": CHECK,
                    "exit_code": 0, "output": "",
                }),
            ),
            "required_check" => handoff.validation.clear(),
            "missing_checkpoint" | "foreign_path" => {
                let HandoffDelivery::NoDiff { evidence } = &mut handoff.candidate.delivery else {
                    panic!("NoDiff")
                };
                evidence.path = if changed == "foreign_path" {
                    "../checkpoint.json"
                } else {
                    "no-diff-handoff/missing.json"
                }
                .into();
            }
            "wrong_task" | "wrong_run" | "checkpoint_base" => {
                let HandoffDelivery::NoDiff { evidence } = &mut handoff.candidate.delivery else {
                    panic!("NoDiff")
                };
                let artifact = fixture
                    .pair
                    .wire
                    .owner
                    .get_task_artifact(&fixture.task, &evidence.path)
                    .unwrap()
                    .unwrap();
                let mut report: Value = serde_json::from_slice(&artifact.content).unwrap();
                report[match changed {
                    "wrong_task" => "task_id",
                    "wrong_run" => "job_run_id",
                    _ => "base_sha",
                }] = json!("another-binding");
                fixture.attach(&evidence.path, report.clone());
                evidence.sha256 = sha256_hex(&serde_json::to_vec(&report).unwrap());
            }
            "removed_tag" => {
                let owner = &fixture.pair.wire.owner;
                let path = find_file(&owner.paths().global_dir, "task.yaml")
                    .or_else(|| find_file(&owner.paths().orbit_dir, "task.yaml"))
                    .unwrap();
                let mut envelope: orbit_types::task::TaskEnvelopeV2 =
                    serde_yaml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
                assert_eq!(envelope.id, fixture.task);
                // Hostile persisted state: ordinary active-claim updates cannot
                // remove tags, but the handoff must still recheck their live value.
                envelope.tags.clear();
                std::fs::write(path, serde_yaml::to_string(&envelope).unwrap()).unwrap();
            }
            "oversize_checkpoint" => {
                let HandoffDelivery::NoDiff { evidence } = &mut handoff.candidate.delivery else {
                    panic!("NoDiff")
                };
                let content = vec![b' '; 1_048_577];
                fixture
                    .bound
                    .attach_claim_validation_log(&evidence.path, content.clone())
                    .unwrap();
                evidence.sha256 = sha256_hex(&content);
                // Exercise the executor's named inline read too, before owner settlement.
                let error = fixture
                    .bound
                    .get_task_artifact(&fixture.task, &evidence.path)
                    .unwrap_err();
                assert!(error.to_string().contains("inline read limit"), "{error}");
            }
            "truncated_checkpoint" => {
                let HandoffDelivery::NoDiff { evidence } = &handoff.candidate.delivery else {
                    panic!("NoDiff")
                };
                let manifest = fixture
                    .pair
                    .wire
                    .owner
                    .get_task_artifact_manifest(&fixture.task)
                    .unwrap();
                let file = manifest
                    .iter()
                    .find(|file| file.path == evidence.path)
                    .unwrap();
                // Resolve the selected manifest blob in the disposable owner store.
                let name = Path::new(&file.blob).file_name().unwrap().to_str().unwrap();
                let blob = find_file(&fixture.pair.wire.owner.paths().global_dir, name)
                    .or_else(|| find_file(&fixture.pair.wire.owner.paths().orbit_dir, name))
                    .unwrap();
                restore_blob = Some((blob.clone(), std::fs::read(&blob).unwrap()));
                std::fs::write(blob, b"{").unwrap();
            }
            _ => unreachable!(),
        }
        let error = fixture.settle(&handoff).expect_err(changed);
        assert!(
            matches!(
                error,
                OrbitError::PolicyDenied(_)
                    | OrbitError::InvalidInput(_)
                    | OrbitError::Execution(_)
                    | OrbitError::Store(_)
                    | OrbitError::TaskBundleCorrupt { .. }
            ),
            "{changed}: {error}"
        );
        let expected = match changed {
            "report" => Some("NoDiff verifier report changed"),
            "missing_checkpoint" => Some("NoDiff verifier report is missing"),
            "wrong_task" | "wrong_run" => Some("this task/run"),
            "checkpoint_base" => Some("checkpoint's base"),
            "removed_tag" => Some("no-diff-expected"),
            "oversize_checkpoint" | "oversize_log" => Some("inline read limit"),
            "missing_log" => Some("missing task artifact missing-validation.json"),
            "wrong_log_run" => Some("validation log must capture the exact run"),
            "truncated_checkpoint" => Some("size mismatch"),
            "foreign_path" => Some("artifact path"),
            _ => None,
        };
        if let Some(expected) = expected {
            assert!(error.to_string().contains(expected), "{changed}: {error}");
        }
        if changed == "rewritten_base" {
            assert!(
                error
                    .to_string()
                    .contains("is not contained in the owner's base"),
                "the refusal names the rewritten base: {error}"
            );
        }
        if let Some((path, content)) = restore_blob {
            // Restore exactly the hostile fixture bytes to inspect unchanged
            // lifecycle state; the corruption refusal itself was asserted above.
            std::fs::write(path, content).unwrap();
        }
        fixture.assert_status("in-progress");
        assert!(
            fixture
                .pair
                .wire
                .owner
                .accepted_task_handoff(&handoff.claim_id)
                .is_err()
        );
        fixture.assert_no_blocked();
    }
}

fn claimed_no_diff_requires_a_verifier_checkpoint_and_passing_logs() {
    let fixture = CleanLeaf::new(false, "done");
    let mut input = fixture.input.clone();
    input["skipped_no_diff_expected"] = json!(true);
    let error = engine_action(&fixture.bound, "claim_validate", &input).unwrap_err();
    assert!(
        error.to_string().contains("skip flag or tag alone"),
        "{error}"
    );
    let mut escaped = fixture.input.clone();
    escaped["implementation"]["no_diff_artifacts"][0]["source_path"] = json!("src/f0.rs");
    let error = engine_action(&fixture.bound, "git_commit", &escaped).unwrap_err();
    assert!(
        error.to_string().contains("outside workspace_root"),
        "scratch import cannot read other checkout files: {error}"
    );
    std::fs::write(
        fixture
            .pair
            .follower_repo
            .join(".orbit/tmp/implementation-validation.json"),
        serde_json::to_vec(&json!({
            "run_id": fixture.leaf, "tested_head": fixture.base, "command": CHECK,
            "exit_code": 1, "output": "failed",
        }))
        .unwrap(),
    )
    .unwrap();
    let error = engine_action(&fixture.bound, "git_commit", &fixture.input).unwrap_err();
    assert!(error.to_string().contains("validation log"), "{error}");
    fixture.assert_status("in-progress");
    for path in ["../no-diff.json", "/no-diff.json"] {
        let error = fixture
            .bound
            .get_task_artifact(&fixture.task, path)
            .unwrap_err();
        assert!(
            error.to_string().contains("artifact path"),
            "{path}: {error}"
        );
    }
    let error = fixture
        .bound
        .get_task_artifact("ORB-99999", "no-diff.json")
        .unwrap_err();
    assert!(
        error.to_string().contains("worker task binding mismatch"),
        "{error}"
    );
    fixture.evidence_reads.lock().unwrap().clear();
    *fixture.lose_evidence_read.lock().unwrap() = true;
    let error = fixture
        .bound
        .get_task_artifact(&fixture.task, "no-diff.json")
        .unwrap_err();
    assert!(
        matches!(error, OrbitError::OutcomeUnknown { .. }),
        "{error}"
    );
    assert_eq!(
        fixture.evidence_reads.lock().unwrap().len(),
        1,
        "an unknown artifact read is neither retried nor replaced by bulk or missing evidence"
    );
    assert!(
        fixture
            .bound
            .get_task_artifact(&fixture.task, "no-diff.json")
            .unwrap()
            .is_some()
    );
    let mut binding = fixture.bound.worker_invocation().unwrap().clone();
    binding.claim_id = "another-claim".into();
    let error = ToOwner(fixture.pair.wire.owner.clone())
        .call(
            "orbit.task.artifact.get",
            json!({"id": fixture.task, "path": "no-diff.json", "_worker_read": "artifact"}),
            ToolSessionContext {
                worker_invocation: Some(binding),
                worker_host_call: true,
                ..Default::default()
            },
        )
        .unwrap_err();
    assert!(
        error.to_string().contains("stale_claim"),
        "foreign claim: {error}"
    );
    fixture.assert_no_blocked();
}

/// The QA-sweep shape that blocked on a follower: a claimed no-diff chore whose
/// `origin/<base>` advances after its run synchronized. `claim_validate` judges
/// the base the run synchronized onto, the handoff records that base rather
/// than the new tip, and the owner accepts and completes it with no operator
/// action, even when the base advances again before landing [ORB-15074].
fn a_claimed_no_diff_chore_completes_after_its_base_advances() {
    for tagged in [false, true] {
        let fixture = if tagged {
            let fixture = CleanLeaf::review_only("done");
            fixture.attach_coverage();
            fixture
        } else {
            CleanLeaf::new(false, "done")
        };
        let tip = fixture.advance_base(1);
        assert_ne!(tip, fixture.base);

        // The shipped pipeline's `validate_no_diff` step is `claim_validate`.
        let handoff = fixture.run_clean_pipeline("task_claimed_pr_pipeline");
        assert_eq!(
            handoff.candidate.base.commit, fixture.base,
            "tagged={tagged}"
        );
        assert_eq!(handoff.candidate.candidate, handoff.candidate.base);
        assert_ne!(handoff.candidate.base.commit, tip);
        let settled = fixture
            .settle(&handoff)
            .expect("owner accepts the synchronized base its base still contains");
        assert_eq!(settled["status"], "review", "tagged={tagged}: {settled}");

        let landing_tip = fixture.advance_base(2);
        let landed = fixture.land(&handoff);
        assert_eq!(landed["evidence"]["delivery"], "no_diff", "{landed}");
        fixture.assert_status("done");
        assert_eq!(
            git(&fixture.pair.owner_repo, &["rev-parse", "main"]).trim(),
            landing_tip,
            "a NoDiff landing moves no branch"
        );
        fixture.assert_no_blocked();
    }
}

/// The same advanced base does not excuse a change: a NoDiff leaf whose
/// worktree carries an uncommitted tracked change or an extra commit after its
/// commit verifier ran is refused at `claim_validate`, naming the change, and
/// the task is not blocked [ORB-15074].
fn a_claimed_no_diff_chore_that_changed_its_worktree_is_refused_by_name() {
    for (tagged, committed, action) in [false, true].into_iter().flat_map(|tagged| {
        [false, true].into_iter().flat_map(move |committed| {
            ["claim_validate", "claim_handoff"]
                .into_iter()
                .map(move |action| (tagged, committed, action))
        })
    }) {
        let fixture = if tagged {
            CleanLeaf::review_only("done")
        } else {
            CleanLeaf::new(false, "done")
        };
        let mut input = fixture.validate_input();
        if action == "claim_handoff" {
            let validated = fixture.action("claim_validate", &input);
            for field in ["candidate", "validation", "no_diff_evidence"] {
                input[field] = validated[field].clone();
            }
        }
        fixture.advance_base(1);
        let follower = &fixture.pair.follower_repo;
        std::fs::write(follower.join("src/f0.rs"), "fn reviewed() {}\n").unwrap();
        if committed {
            git(follower, &["commit", "-q", "-am", "Chore edits code"]);
        }
        let error = engine_action(&fixture.bound, action, &input).unwrap_err();
        let named = if action == "claim_handoff" {
            if committed {
                "carries commits beyond base"
            } else {
                "worktree is not clean"
            }
        } else if committed {
            "Chore edits code"
        } else {
            "src/f0.rs"
        };
        assert!(
            matches!(
                error,
                OrbitError::PolicyDenied(_) | OrbitError::Execution(_)
            ) && error.to_string().contains(named),
            "{action}: tagged={tagged} committed={committed}: {error}"
        );
        fixture.assert_status("in-progress");
        fixture.assert_no_handoff();
        fixture.assert_no_blocked();
    }
}

/// A clean-base NoDiff handoff replays the gate the implementer's sandbox
/// deferred, even when only `review.baseline_commands` names it, and attaches
/// its log beside the required command's. Before, only the required commands
/// ran and the deferral was silently dropped [ORB-15287].
fn a_claimed_no_diff_handoff_replays_a_baseline_only_deferred_gate() {
    for job in NO_DIFF_ROUTES {
        let mut fixture = CleanLeaf::gated("done");
        fixture.attach_coverage();
        fixture.hand_off_deferral();
        let handoff = fixture.run_clean_pipeline(job);
        let gate = fixture.gate.as_ref().unwrap();
        assert_eq!(gate.runs(), 1, "{job}: the deferred gate replays once");
        assert_eq!(
            fixture.owner_validated_commands(&handoff),
            vec![CHECK.to_string(), gate.command()],
            "{job}: the replay's log is attached beside the required command's"
        );
        assert!(
            matches!(handoff.candidate.delivery, HandoffDelivery::NoDiff { .. }),
            "{job}: the clean-base checkpoint is preserved"
        );
        let settled = fixture
            .settle(&handoff)
            .expect("the owner accepts the replayed handoff");
        assert_eq!(settled["status"], "review", "{job}: {settled}");
        fixture.assert_no_blocked();
    }
}

/// A replay that still defers on the validating host never ran the paths it
/// was handed off for: the validation environment's failure, with no passing
/// log attached and no handoff recorded.
fn a_claimed_no_diff_replay_that_still_defers_is_refused() {
    for job in NO_DIFF_ROUTES {
        let mut fixture = CleanLeaf::gated("done");
        fixture.hand_off_deferral();
        fixture.gate.as_ref().unwrap().defers();
        let refusal = fixture.refused_pipeline(job);
        assert!(
            orbit_types::workflow::is_validation_environment_failure(None, Some(&refusal)),
            "{job}: {refusal}"
        );
        assert_eq!(fixture.gate.as_ref().unwrap().runs(), 1, "{job}");
    }
}

/// A replay that reports no executed test replayed nothing, so it cannot
/// carry a deferral.
fn a_claimed_no_diff_replay_that_ran_no_tests_is_refused() {
    for job in NO_DIFF_ROUTES {
        let mut fixture = CleanLeaf::gated("done");
        fixture.hand_off_deferral();
        fixture.gate.as_ref().unwrap().executes(0);
        let refusal = fixture.refused_pipeline(job);
        assert!(
            !orbit_types::workflow::is_validation_environment_failure(None, Some(&refusal))
                && refusal.contains("replayed nothing"),
            "{job}: a policy denial, not an environment failure: {refusal}"
        );
        assert_eq!(fixture.gate.as_ref().unwrap().runs(), 1, "{job}");
    }
}

/// With nothing handed off, a gate named only as a baseline command stays
/// unrun and the ordinary clean-base handoff delivers as before.
fn a_claimed_no_diff_handoff_without_a_deferral_replays_nothing() {
    for job in NO_DIFF_ROUTES {
        let fixture = CleanLeaf::gated("done");
        fixture.attach_coverage();
        let handoff = fixture.run_clean_pipeline(job);
        assert_eq!(fixture.gate.as_ref().unwrap().runs(), 0, "{job}");
        assert_eq!(
            fixture.owner_validated_commands(&handoff),
            vec![CHECK.to_string()],
            "{job}"
        );
        let settled = fixture.settle(&handoff).expect("the owner accepts it");
        assert_eq!(settled["status"], "review", "{job}: {settled}");
        fixture.assert_no_blocked();
    }
}

/// [ORB-15622] A replica whose `review.baseline_commands` lacks a gate the
/// owner lists accepts its implementer's Seatbelt or Bubblewrap deferral of
/// that gate, because the leaf holds the owner's gates its claim captured, and
/// `claim_validate` replays it. Before, the replica's own empty list refused
/// every such deferral and the leaf ended held.
fn a_replica_lacking_the_owners_baseline_gate_accepts_and_replays_its_deferral() {
    for (job, seatbelt) in NO_DIFF_ROUTES.into_iter().zip([true, false]) {
        let mut fixture = CleanLeaf::gated_on("done", GateOn::OwnerOnly);
        let gate = fixture.gate.as_ref().unwrap().command();
        assert!(
            RuntimeHost::review_baseline_commands(&fixture.pair.follower).is_empty(),
            "{job}: the replica's own configuration names no baseline gate"
        );
        let gates = RuntimeHost::validation_gates(&fixture.bound);
        assert_eq!(
            gates.source,
            orbit_types::workflow::ValidationGateSource::OwnerContract,
            "{job}"
        );
        assert_eq!(gates.gates.baseline_commands, vec![gate.clone()], "{job}");
        let record = fixture.deferral(seatbelt);
        check_deferral(&record, &gates, &fixture.base)
            .unwrap_or_else(|refusal| panic!("{job} seatbelt={seatbelt}: {refusal}"));
        fixture.input["implementation"]["deferred_sandbox_validation"] = record;
        fixture.attach_coverage();
        let handoff = fixture.run_clean_pipeline(job);
        assert_eq!(
            fixture.gate.as_ref().unwrap().runs(),
            1,
            "{job}: replayed once"
        );
        assert_eq!(
            fixture.owner_validated_commands(&handoff),
            vec![CHECK.to_string(), gate],
            "{job}"
        );
        let settled = fixture.settle(&handoff).expect("the owner accepts it");
        assert_eq!(settled["status"], "review", "{job}: {settled}");
    }
}

/// [ORB-15622] The inverse drift: a gate only the replica lists is not one
/// of the owner's, so a deferral of it is refused, and the refusal names the
/// gate list it checked and that it came from the claim's admission contract.
fn a_deferral_of_a_replica_only_gate_is_refused_naming_the_owners_gates() {
    let checked = format!(
        "the gates checked are [`{CHECK}`] from the owner's gates captured in this claim's \
         admission contract"
    );
    for job in NO_DIFF_ROUTES {
        let mut fixture = CleanLeaf::gated_on("done", GateOn::FollowerOnly);
        let refusal = check_deferral(
            &fixture.deferral(false),
            &RuntimeHost::validation_gates(&fixture.bound),
            &fixture.base,
        )
        .unwrap_err();
        assert!(refusal.contains(&checked), "{job}: {refusal}");
        fixture.hand_off_deferral();
        // The policy denial is raised before any command runs, so unlike a
        // replay that ran, it leaves no validation log to point at.
        let refused = fixture.refusal_of(job);
        assert!(refused.contains(&checked), "{job}: {refused}");
        assert!(
            fixture.pair.admission(&fixture.leaf).settlement.is_none(),
            "{job}: a refused leaf records no handoff"
        );
        let artifacts = fixture.owner_artifacts();
        assert!(
            artifacts
                .iter()
                .all(|artifact| !artifact.path.starts_with("validation/")),
            "{job}: nothing ran, so nothing is logged"
        );
        fixture.assert_no_blocked();
        assert_eq!(fixture.gate.as_ref().unwrap().runs(), 0, "{job}");
    }
}

/// [ORB-15427] A claimed clean-base leaf whose required command fails only
/// with tests its base fails too hands off a known-red log instead of
/// releasing the claim, and the owner takes none of it on the leaf's word:
/// settling the handoff runs the command on the base on the owner itself.
/// When that run fails the same test, the handoff is accepted and the
/// owner's task history names the test. When it passes, the owner releases
/// the claim instead [ORB-15566], with nothing recorded as known red.
#[test]
fn a_claimed_known_red_log_is_accepted_only_on_the_owners_own_base_run() {
    use orbit_types::workflow::KNOWN_RED_VALIDATION_EVENT;

    if !isolated(
        module_path!(),
        "a_claimed_known_red_log_is_accepted_only_on_the_owners_own_base_run",
    ) {
        return;
    }
    for owner_base_red in [true, false] {
        let red = RedCheck::new();
        let fixture = CleanLeaf::red(&red);
        fixture.attach_coverage();
        let handoff = fixture.run_clean_pipeline("task_claimed_pr_pipeline");
        let owner = &fixture.pair.wire.owner;
        let artifacts = RuntimeHost::get_task_artifacts(owner, &fixture.task).unwrap();
        let logs = handoff
            .validation
            .iter()
            .map(|reference| {
                let artifact = artifacts
                    .iter()
                    .find(|artifact| artifact.path == reference.path)
                    .unwrap();
                serde_json::from_slice::<HandoffValidationLog>(&artifact.content).unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(logs.len(), 1, "{logs:?}");
        assert_eq!(logs[0].command, red.command());
        assert_eq!(logs[0].exit_code, 3, "the log keeps the exit status");
        let report = logs[0]
            .known_red
            .as_ref()
            .expect("the leaf reports known red");
        assert_eq!(report.base_sha, fixture.base);
        assert_eq!(
            report.failures,
            BTreeSet::from([RedCheck::FAILURE.to_string()])
        );

        let leaf_runs = red.runs();
        if !owner_base_red {
            red.pass();
        }
        let settled = fixture.settle(&handoff);
        assert!(
            red.runs() > leaf_runs,
            "owner_base_red={owner_base_red}: the owner ran the base itself"
        );
        let known_red = fixture.pair.owner_task(&fixture.task)["history"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|event| event["event"] == KNOWN_RED_VALIDATION_EVENT)
            .map(known_red_decision)
            .collect::<Vec<_>>();
        if owner_base_red {
            let settled = settled.expect("the owner's base run fails the reported test");
            assert_eq!(settled["status"], "review", "{settled}");
            assert_eq!(known_red.len(), 1, "{known_red:?}");
            assert_eq!(known_red[0].command, red.command());
            assert_eq!(known_red[0].base_sha, fixture.base);
            assert_eq!(
                known_red[0].failures.identities,
                vec![RedCheck::FAILURE.to_string()]
            );
        } else {
            let error = settled
                .expect_err("the owner's base run passes the reported test")
                .to_string();
            assert!(
                error.contains("known_red_contradicted")
                    && error.contains(&format!("base {} passes it", fixture.base)),
                "{error}"
            );
            fixture.assert_status("backlog");
            assert!(known_red.is_empty(), "{known_red:?}");
            assert!(owner.accepted_task_handoff(&handoff.claim_id).is_err());
        }
        fixture.assert_no_blocked();
    }
}

/// [ORB-16432] A macOS leaf's known-red report is not contradicted by a Linux
/// owner's base run. The two hosts are different validation environments, so
/// the owner does not run the base, does not release `known_red_contradicted`,
/// and accepts the follower's failures. The handoff stays the delivery, so a
/// pull request the leaf opened is not abandoned for a later attempt.
#[test]
fn a_macos_leaf_known_red_report_is_not_contradicted_by_a_linux_owner_base() {
    use orbit_types::task::HostOs;
    use orbit_types::workflow::KNOWN_RED_VALIDATION_EVENT;

    if !isolated(
        module_path!(),
        "a_macos_leaf_known_red_report_is_not_contradicted_by_a_linux_owner_base",
    ) {
        return;
    }
    let red = RedCheck::new();
    let fixture = CleanLeaf::red_on(&red, Some(HostOs::Linux), Some(HostOs::Macos));
    let admitted = fixture.pair.admission(&fixture.leaf);
    assert_eq!(admitted.request.os, Some(HostOs::Macos));
    fixture.attach_coverage();
    let handoff = fixture.run_clean_pipeline("task_claimed_pr_pipeline");
    let leaf_runs = red.runs();
    // A Linux pass of the same command would be a contradiction on one OS.
    red.pass();
    let settled = fixture
        .settle(&handoff)
        .expect("a different OS does not contradict the follower's base evidence");
    assert_eq!(
        red.runs(),
        leaf_runs,
        "the owner does not run its own base for another OS"
    );
    assert_eq!(settled["status"], "review", "{settled}");
    assert_ne!(fixture.pair.owner_status(&fixture.task), "backlog");
    let claim = fixture.pair.owner_claim(&handoff.claim_id);
    assert_eq!(claim["claim"]["phase"], "handed_off", "{claim}");
    let accepted = fixture.accepted(&handoff);
    assert_eq!(
        accepted.handoff.candidate.delivery, handoff.candidate.delivery,
        "the leaf's delivery stays this attempt"
    );
    let task = fixture.pair.owner_task(&fixture.task);
    let events = task["history"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| event["event"] == KNOWN_RED_VALIDATION_EVENT)
        .collect::<Vec<_>>();
    assert_eq!(events.len(), 1, "{events:?}");
    let known = known_red_decision(events[0]);
    let note = events[0]["note"].as_str().unwrap();
    assert_eq!(known.command, red.command());
    assert_eq!(known.base_sha, fixture.base);
    assert_eq!(
        known.failures.identities,
        vec![RedCheck::FAILURE.to_string()]
    );
    assert!(
        note.contains("macos") && note.contains("linux"),
        "the acceptance names the OS mismatch: {note}"
    );
    assert!(
        task["history"]
            .as_array()
            .unwrap()
            .iter()
            .all(|event| event["event"] != "claim_released"
                && !event["note"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("known_red_contradicted")),
        "{task}"
    );
    fixture.assert_no_blocked();
}

/// A published candidate may descend from the follower's reported base even
/// when that base is off the owner's admitted branch. Cross-OS settlement must
/// refuse that report before accepting the handoff or running any command.
#[test]
#[cfg(unix)]
fn a_cross_os_known_red_report_from_off_the_owners_branch_is_refused() {
    use orbit_types::task::HostOs;
    use orbit_types::workflow::KNOWN_RED_VALIDATION_EVENT;
    use orbit_types::workflow::handoff::HandoffKnownRed;
    use std::os::unix::fs::PermissionsExt;

    if !isolated(
        module_path!(),
        "a_cross_os_known_red_report_from_off_the_owners_branch_is_refused",
    ) {
        return;
    }
    let red = RedCheck::new();
    let config = format!(
        "[workflow]\nrequired_validation_commands = [\"{}\"]\n",
        red.command()
    );
    let mut pair = Pair::with_owner_host(&config, "", &[None], |owner| {
        owner.with_host_os(Some(HostOs::Linux))
    });
    pair.follower = pair.follower.clone().with_host_os(Some(HostOs::Macos));
    let task = &pair.tasks[0];
    let owner_base = prepare_clean_owner(&pair, task);
    let drain = pair.run_drain();
    let leaf = pair.launched_leaf(&drain, 1, std::process::id());
    let admission = pair.admission(&leaf);
    assert_eq!(admission.request.os, Some(HostOs::Macos));
    assert_eq!(admission.request.ship.base_branch, "main");
    prepare_clean_follower(&pair, task, &owner_base);
    let repo = &pair.follower_repo;
    git(
        repo,
        &["commit", "-q", "--allow-empty", "-m", "Foreign base"],
    );
    let foreign_base = super::before_pr::revision(repo, "HEAD");
    std::fs::write(repo.join("src/f0.rs"), "fn delivered() {}\n").unwrap();
    git(repo, &["commit", "-q", "-am", "Deliver on foreign base"]);
    let mut delivered = handoff(&admission);
    delivered.candidate.base = foreign_base;
    delivered.candidate.candidate = super::before_pr::revision(repo, "HEAD");
    git(repo, &["push", "-q", "origin", "HEAD"]);

    // Only the provider response is substituted. Settlement reads the real
    // published objects, checks their ancestry and judges the pinned log.
    let home = PathBuf::from(std::env::var("HOME").unwrap());
    std::fs::create_dir_all(home.join("bin")).unwrap();
    let gh = home.join("bin/gh");
    std::fs::write(
        &gh,
        format!(
            "#!/bin/sh\ncat <<'JSON'\n{}\nJSON\n",
            json!({
                "number": 42, "state": "OPEN", "mergedAt": null,
                "headRefName": delivered.candidate.source_branch,
                "headRefOid": delivered.candidate.candidate.commit,
                "baseRefName": "main", "mergeStateStatus": "CLEAN",
                "url": "https://github.com/owner/repository/pull/42",
            })
        ),
    )
    .unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    // The fixture's URL redirect gives the published bare origin its own
    // repository identity; carry the identity the public observer reads.
    delivered.candidate = orbit_engine::observe_published_candidate(
        &pair.wire.owner,
        &pair.owner_repo,
        &delivered.candidate,
    )
    .unwrap();
    let log = HandoffValidationLog {
        schema_version: 1,
        workspace_id: delivered.workspace_id.clone(),
        task_id: task.clone(),
        claim_id: delivered.claim_id.clone(),
        machine_id: delivered.machine_id.clone(),
        run_id: leaf.clone(),
        candidate: delivered.candidate.clone(),
        tested_head: delivered.candidate.candidate.commit.clone(),
        command: red.command(),
        exit_code: 3,
        output: "test suite::broken ... FAILED".into(),
        known_red: Some(HandoffKnownRed {
            base_sha: delivered.candidate.base.commit.clone(),
            selection: None,
            failures: BTreeSet::from([RedCheck::FAILURE.to_string()]),
        }),
        flaky: None,
    };
    let artifact = TaskArtifact {
        path: "validation/foreign-base.json".into(),
        content: serde_json::to_vec(&log).unwrap(),
        media_type: "application/json".into(),
        created_by: None,
    };
    delivered.validation.push(HandoffArtifactRef {
        path: artifact.path.clone(),
        sha256: sha256_hex(&artifact.content),
    });
    let context = ClaimInvocation::trusted_worker(
        task.clone(),
        delivered.claim_id.clone(),
        FOLLOWER.into(),
        Some(ClaimRun {
            machine_id: FOLLOWER.into(),
            run_id: leaf.clone(),
        }),
    );
    pair.wire
        .owner
        .mutate_execution_claim(
            Some(&context),
            "foreign-base-log",
            &ClaimMutation::Evidence(ClaimEvidence {
                artifacts: vec![artifact],
                ..Default::default()
            }),
        )
        .unwrap();
    let error = pair
        .wire
        .owner
        .run_tool_with_context_and_role(
            "orbit.drain.claim.settle",
            json!({"claim_id": delivered.claim_id, "run_id": leaf,
            "settlement": ClaimMutation::AcceptHandoff(delivered.clone())}),
            Role::Admin,
            ToolContext {
                session_context: ToolSessionContext {
                    caller_machine_id: Some(FOLLOWER.into()),
                    process_machine_id: Some(OWNER.into()),
                    transport: Some(McpTransport::SshMcp),
                    effective_capabilities: BTreeSet::from([McpCapability::Agent]),
                    ..ToolSessionContext::default()
                },
                ..ToolContext::default()
            },
        )
        .expect_err("an off-branch base must not authorize cross-OS known red")
        .to_string();
    assert!(
        error.contains("known_red_unverified") && error.contains("not in the history"),
        "{error}"
    );
    assert_eq!(red.runs(), 0, "an off-branch report runs no owner command");
    assert_eq!(pair.owner_status(task), "in-progress");
    assert!(
        pair.wire
            .owner
            .accepted_task_handoff(&delivered.claim_id)
            .is_err()
    );
    assert!(
        pair.owner_task(task)["history"]
            .as_array()
            .unwrap()
            .iter()
            .all(|event| event["event"] != KNOWN_RED_VALIDATION_EVENT)
    );
}

/// [ORB-15566] A known-red handoff the owner's own base run contradicts can
/// never be accepted, so the owner ends the claim rather than refusing the
/// settlement while holding it: the base passing the reported test, or
/// failing without it, releases the claim as a `transient` failure naming
/// the owner's verdict, and the task returns to the backlog to be pulled
/// again. The follower reconciles the refusal against the ended claim and
/// closes its pending settlement obsolete, freeing its slot. An inconclusive
/// owner check (here, a tool the base run lacks) still refuses while the
/// owner holds the claim, and the follower retries it after its backoff.
fn a_known_red_handoff_the_owners_base_run_contradicts_ends_the_claim() {
    let cases = [
        (
            "passes",
            "echo 'test suite::broken ... ok'\nexit 0",
            Some("passes it"),
        ),
        (
            "fails without it",
            "echo 'test suite::broken ... ok'\necho 'test suite::other ... FAILED'\nexit 3",
            Some("does not fail `test suite::broken`"),
        ),
        (
            "lacks a tool",
            "echo 'sh: 1: frobnicate: not found' >&2\nexit 127",
            None,
        ),
    ];
    for (case, base, contradiction) in cases {
        let red = RedCheck::new();
        let fixture = CleanLeaf::red(&red);
        fixture.attach_coverage();
        let handoff = fixture.run_clean_pipeline("task_claimed_pr_pipeline");
        let pair = &fixture.pair;
        assert_eq!(
            pair.admission(&fixture.leaf).phase,
            LocalPullPhase::Settling,
            "{case}: the handoff waits for delivery"
        );
        let leaf_runs = red.runs();
        red.runs_as(base);

        pair.follower.deliver_recorded_pull_settlements();
        assert_eq!(
            pair.wire.calls("orbit.drain.claim.settle").len(),
            1,
            "{case}"
        );
        assert_eq!(
            red.runs(),
            leaf_runs + 1,
            "{case}: the owner ran the base itself"
        );
        let claim = pair.owner_claim(&handoff.claim_id);
        let follower = pair
            .follower
            .pull_leaf_claim(&fixture.leaf)
            .unwrap()
            .unwrap();
        assert!(
            pair.wire
                .owner
                .accepted_task_handoff(&handoff.claim_id)
                .is_err(),
            "{case}: the leaf's log is never accepted"
        );
        let Some(verdict) = contradiction else {
            assert_eq!(claim["claim"]["phase"], "running", "{case}: {claim:#}");
            assert_eq!(pair.owner_status(&fixture.task), "in-progress", "{case}");
            assert_eq!(follower.settlement_phase, "settling", "{case}");
            let refused = follower.settlement_refusal.expect("a recorded refusal");
            assert!(
                refused.reason.contains("known_red_unverified")
                    && refused.reason.contains("lacked a tool"),
                "{case}: {refused:?}"
            );
            assert_eq!(pair.follower.pending_pull_settlements().unwrap().count, 1);

            // The follower delivers the same settlement again once its
            // backoff elapses, and the owner answers the same.
            pair.elapse_settlement_backoff(&fixture.leaf);
            pair.follower.deliver_recorded_pull_settlements();
            assert_eq!(
                pair.wire.calls("orbit.drain.claim.settle").len(),
                2,
                "{case}"
            );
            let follower = pair
                .follower
                .pull_leaf_claim(&fixture.leaf)
                .unwrap()
                .unwrap();
            assert_eq!(follower.settlement_phase, "settling", "{case}");
            assert_eq!(
                follower.settlement_refusal.map(|refused| refused.refusals),
                Some(2),
                "{case}"
            );
            assert_eq!(pair.owner_status(&fixture.task), "in-progress", "{case}");
            continue;
        };
        assert_eq!(claim["claim"]["phase"], "revoked", "{case}: {claim:#}");
        let task = pair.owner_task(&fixture.task);
        assert_eq!(task["status"], "backlog", "{case}: {task:#}");
        let note = task["history"]
            .as_array()
            .unwrap()
            .iter()
            .rfind(|event| event["event"] == "claim_released")
            .and_then(|event| event["note"].as_str())
            .unwrap_or_default();
        assert!(
            note.contains("known_red_contradicted")
                && note.contains(&format!("base {} {verdict}", fixture.base)),
            "{case}: the owner's verdict is the release reason: {task:#}"
        );
        assert_eq!(follower.settlement_phase, "settled", "{case}");
        let refusal = follower.refusal.expect("closed as obsolete");
        assert!(
            refusal.contains("known_red_contradicted") && refusal.contains("already ended"),
            "{case}: {refusal}"
        );
        assert_eq!(
            pair.follower.pending_pull_settlements().unwrap().count,
            0,
            "{case}: the slot is free"
        );
        fixture.assert_no_blocked();
    }
}

/// A base run that cannot finish within settlement's budget is cut once,
/// then retries reuse its durable inconclusive outcome, including on a newly
/// opened owner session. Partial failed-test output never admits a handoff.
///
/// The budget is a generous ceiling for reaching the base, which host load can
/// stretch. The cut comes from the supervisor's clock once the stalled base
/// reports it is running, so no wall time is waited out [ORB-16402].
#[test]
#[cfg(unix)]
fn a_slow_known_red_base_run_converges_to_one_cut_over_settle_retries() {
    use orbit_common::test_env::{SupervisorSeam, with_supervision_clock, with_supervisor_seam};
    use std::{cell::Cell, rc::Rc, time::Instant};

    if !isolated(
        module_path!(),
        "a_slow_known_red_base_run_converges_to_one_cut_over_settle_retries",
    ) {
        return;
    }
    let red = RedCheck::new();
    let fixture = CleanLeaf::red(&red);
    fixture.attach_coverage();
    let handoff = fixture.run_clean_pipeline("task_claimed_pr_pipeline");
    let leaf_runs = red.runs();

    // No timing race or load: the base prints the claimed failure and reports
    // readiness, then blocks on a FIFO with no writer until the supervisor cuts
    // its process group. The cut happens when the supervisor's clock passes the
    // attempt's limit, which this test advances only at that readiness point.
    let budget = Duration::from_secs(600);
    let fifo = red._dir.path().join("wait");
    let ready = red._dir.path().join("ready");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    std::fs::write(
        &red.script,
        format!(
            "echo ran >> '{}'\necho 'test suite::broken ... FAILED'\ntouch '{}'\nread ignored < '{}'\nexit 3\n",
            red.runs.display(),
            ready.display(),
            fifo.display(),
        ),
    )
    .unwrap();
    let offset = Rc::new(Cell::new(Duration::ZERO));
    let (clock_offset, wait_offset) = (Rc::clone(&offset), Rc::clone(&offset));
    let seam = SupervisorSeam {
        wait: Some(Box::new(move |child| {
            if ready.exists() {
                std::fs::remove_file(&ready).unwrap();
                // Advancing the clock here takes effect on the supervisor's
                // next deadline check, which follows this poll.
                wait_offset.set(budget);
                return Ok(None);
            }
            child.try_wait()
        })),
        ..SupervisorSeam::default()
    };
    let settle = |owner: &OrbitRuntime| {
        owner
            .run_tool_with_context_and_role(
                "orbit.drain.claim.settle",
                json!({
                    "claim_id": handoff.claim_id, "run_id": fixture.leaf,
                    "settlement": ClaimMutation::AcceptHandoff(handoff.clone()),
                }),
                Role::Admin,
                ToolContext {
                    session_context: ToolSessionContext {
                        caller_machine_id: Some(FOLLOWER.to_string()),
                        process_machine_id: Some(OWNER.to_string()),
                        transport: Some(McpTransport::SshMcp),
                        effective_capabilities: BTreeSet::from([McpCapability::Agent]),
                        ..ToolSessionContext::default()
                    },
                    ..ToolContext::default()
                },
            )
            .expect_err("a cut base never confirms known red")
            .to_string()
    };
    let owner = fixture
        .pair
        .wire
        .owner
        .clone()
        .with_claim_known_red_budget(budget);
    let error = with_supervision_clock(
        move || Instant::now() + clock_offset.get(),
        || with_supervisor_seam(seam, || settle(&owner)),
    );
    assert!(
        error.contains("known_red_unverified") && error.contains("comparison deadline"),
        "{error}"
    );
    assert_eq!(
        offset.get(),
        budget,
        "the stalled base reached readiness before its deadline cut"
    );
    assert_eq!(
        red.runs(),
        leaf_runs + 1,
        "the owner starts just one base run"
    );

    // A command now able to complete still must not restart on settlement
    // retry. Reopening the runtime rules out a process-local refusal cache.
    std::fs::write(
        &red.script,
        format!(
            "echo ran >> '{}'\necho 'test suite::broken ... ok'\nexit 0\n",
            red.runs.display(),
        ),
    )
    .unwrap();
    for _ in 0..2 {
        let reopened =
            OrbitRuntime::from_roots(&owner.paths().global_dir, &owner.paths().orbit_dir)
                .unwrap()
                .with_automation_machine_identity(Some(OWNER.to_string()))
                .with_claim_known_red_budget(budget);
        assert_eq!(settle(&reopened), error);
        assert_eq!(red.runs(), leaf_runs + 1, "settle retries reuse the cut");
    }
    fixture.assert_status("in-progress");
    assert!(owner.accepted_task_handoff(&handoff.claim_id).is_err());
    assert!(
        fixture.pair.owner_task(&fixture.task)["history"]
            .as_array()
            .unwrap()
            .iter()
            .all(|event| event["event"] != orbit_types::workflow::KNOWN_RED_VALIDATION_EVENT)
    );
    fixture.assert_no_blocked();
}

/// [ORB-15747] A claimed leaf that finds its base already resolves the task,
/// shown only by a test that skips inside the agent sandbox, names that test
/// as its already-resolved outcome. Its claim host replays it on the base
/// outside the agent sandbox, the passing log joins the handoff beside the
/// required command's, and the owner completes the task without a PR.
/// Before, an empty diff gave the affected-test gate nothing to run, and the
/// task blocked until an operator ran the test by hand.
#[cfg(unix)]
fn a_claimed_already_resolved_base_replays_its_named_tests_and_completes_without_a_pr() {
    for job in NO_DIFF_ROUTES {
        let replay = Replay::new();
        let fixture = CleanLeaf::resolved(&replay, &[Replay::SELECTOR]);
        let handoff = fixture.run_clean_pipeline(job);
        assert_eq!(
            replay.runs(),
            vec![format!(
                "test -p orbit-cli --test process {} -- --nocapture",
                Replay::TEST
            )],
            "{job}: the named test replays once, through the host's grammar"
        );
        assert_eq!(handoff.candidate.candidate, handoff.candidate.base, "{job}");
        assert_eq!(handoff.candidate.base.commit, fixture.base, "{job}");
        assert_eq!(
            fixture.owner_validated_commands(&handoff),
            vec![CHECK.to_string(), Replay::host_command()],
            "{job}: the replay is recorded beside the required command"
        );
        let artifacts = fixture.owner_artifacts();
        let replayed = artifacts
            .iter()
            .find(|artifact| artifact.path == handoff.validation[1].path)
            .expect("the owner holds the replay log the handoff pins");
        let log: HandoffValidationLog = serde_json::from_slice(&replayed.content).unwrap();
        assert_eq!(log.tested_head, fixture.base, "{job}");
        assert!(
            log.output.contains("test result: ok. 1 passed"),
            "{job}: {log:?}"
        );

        let settled = fixture.settle(&handoff).expect("the owner accepts it");
        assert_eq!(settled["status"], "review", "{job}: {settled}");
        fixture.land(&handoff);
        assert_eq!(fixture.pair.owner_status(&fixture.task), "done", "{job}");
        fixture.assert_no_blocked();
    }
}

/// [ORB-15747] A named test that still fails natively fails the step with
/// its output, before any handoff, and the leaf's settlement blocks the task
/// on the owner with that output; the full log is on the owner's task.
#[cfg(unix)]
fn a_failing_already_resolved_replay_blocks_the_task_with_its_output() {
    let replay = Replay::new();
    replay.fails();
    let fixture = CleanLeaf::resolved(&replay, &[Replay::SELECTOR]);
    let input = fixture.validate_input();
    let error = engine_action(&fixture.bound, "claim_validate", &input).unwrap_err();
    let message = error.to_string();
    assert!(
        matches!(error, OrbitError::Execution(_))
            && message.contains("test_failed")
            && message.contains("interrupt was not reported"),
        "{message}"
    );
    assert_eq!(replay.runs().len(), 1);
    assert!(
        fixture.pair.admission(&fixture.leaf).settlement.is_none(),
        "a failed replay records no handoff"
    );
    let artifacts = fixture.owner_artifacts();
    let failed = artifacts
        .iter()
        .find(|artifact| artifact.path.ends_with("/already-resolved-0.failed.json"))
        .expect("the failed replay's log is on the owner's task");
    assert!(message.contains(&failed.path), "{message}");
    let log: Value = serde_json::from_slice(&failed.content).unwrap();
    assert_eq!(log["exit_code"], 101, "{log}");
    assert_eq!(log["tested_head"], fixture.base.as_str(), "{log}");
    assert!(
        log["output"]
            .as_str()
            .is_some_and(|output| output.contains("interrupt was not reported")),
        "{log}"
    );

    // The leaf ends on that step's failure, and its settlement blocks the task.
    fixture.pair.leaf_fails_with(&fixture.leaf, &message);
    let pass = fixture
        .pair
        .pass_over(&fixture.drain, json!({"window_expired": true}));
    assert_eq!(
        fixture.pair.owner_status(&fixture.task),
        "blocked",
        "{pass}"
    );
    let task = fixture.pair.owner_task(&fixture.task).to_string();
    assert!(
        task.contains("test result: FAILED. 0 passed; 1 failed"),
        "the owner's task names the failing output: {task}"
    );
}

/// [ORB-15747] An already-resolved outcome that would prove nothing is
/// refused rather than settled: no selectors, a selector outside the `cargo
/// test` grammar (refused before anything runs), a replay that runs no test
/// or skips its path, and a declaration on a candidate that changes its base.
#[test]
#[cfg(unix)]
fn an_already_resolved_claim_that_runs_no_named_test_is_refused() {
    if !isolated(
        module_path!(),
        "an_already_resolved_claim_that_runs_no_named_test_is_refused",
    ) {
        return;
    }
    for (selectors, refusal) in [
        (vec![], "names no test selectors"),
        (vec!["make ci-test-affected"], "command_not_allowed"),
        (
            vec!["cargo test -p orbit-cli --test process x;touch ran"],
            "shell_metacharacter",
        ),
        (vec![Replay::SELECTOR, Replay::SELECTOR], "twice"),
    ] {
        let replay = Replay::new();
        let fixture = CleanLeaf::resolved(&replay, &selectors);
        let input = fixture.validate_input();
        let error = engine_action(&fixture.bound, "claim_validate", &input).unwrap_err();
        assert!(
            matches!(error, OrbitError::PolicyDenied(_)) && error.to_string().contains(refusal),
            "{selectors:?}: {error}"
        );
        assert!(replay.runs().is_empty(), "{selectors:?}: nothing replays");
        let artifacts = fixture.owner_artifacts();
        assert!(
            artifacts
                .iter()
                .all(|artifact| !artifact.path.starts_with("validation/")),
            "{selectors:?}: refused before any command runs"
        );
        fixture.assert_no_handoff();
    }

    for (behave, refusal) in [
        (Replay::passes as fn(&Replay, u64), "no_tests_ran"),
        (|replay: &Replay, _| replay.skips(), "self_skipped"),
    ] {
        let replay = Replay::new();
        behave(&replay, 0);
        let fixture = CleanLeaf::resolved(&replay, &[Replay::SELECTOR]);
        let input = fixture.validate_input();
        let error = engine_action(&fixture.bound, "claim_validate", &input).unwrap_err();
        assert!(
            matches!(error, OrbitError::PolicyDenied(_)) && error.to_string().contains(refusal),
            "{refusal}: {error}"
        );
        assert_eq!(replay.runs().len(), 1, "{refusal}");
        assert!(
            fixture.pair.admission(&fixture.leaf).settlement.is_none(),
            "{refusal}: no handoff"
        );
    }

    // Off the NoDiff route, the candidate changed its base.
    let replay = Replay::new();
    let fixture = CleanLeaf::resolved(&replay, &[Replay::SELECTOR]);
    let error = engine_action(&fixture.bound, "claim_validate", &fixture.input).unwrap_err();
    assert!(
        matches!(error, OrbitError::PolicyDenied(_)) && error.to_string().contains("changes it"),
        "{error}"
    );
    assert!(replay.runs().is_empty());
}

// Related boundary scenarios share the isolated table fixture.
crate::scenario_table!(
    verified_no_diff_and_already_satisfied_base_complete_without_a_pr_scenarios,
    verified_no_diff_and_already_satisfied_base_complete_without_a_pr,
    a_follower_claims_no_diff_expected_work_files_findings_and_completes_without_a_pr
);

crate::scenario_table!(
    a_claimed_no_diff_expected_review_without_a_report_settles_like_the_owner_scenarios,
    an_owner_accepts_a_tagged_skip_that_still_carries_logs_and_checks_them,
    a_claimed_no_diff_expected_task_that_changed_the_worktree_is_refused
);

crate::scenario_table!(
    owner_refuses_a_no_diff_expected_skip_for_an_untagged_task_scenarios,
    owner_refuses_a_no_diff_expected_skip_for_an_untagged_task,
    a_claimed_delivery_review_files_regression_findings_and_hands_off_the_rest,
    a_published_candidates_string_findings_are_normalized_at_handoff
);

crate::scenario_table!(
    no_diff_retains_owner_completion_authority_scenarios,
    no_diff_retains_owner_completion_authority,
    a_claimed_implementers_regression_culprit_is_recorded_at_handoff,
    owner_refuses_changed_no_diff_evidence_base_or_missing_required_checks_without_blocking,
    claimed_no_diff_requires_a_verifier_checkpoint_and_passing_logs
);

crate::scenario_table!(
    a_claimed_no_diff_chore_completes_after_its_base_advances_scenarios,
    a_claimed_no_diff_chore_completes_after_its_base_advances,
    a_claimed_no_diff_chore_that_changed_its_worktree_is_refused_by_name
);

crate::scenario_table!(
    a_claimed_no_diff_handoff_replays_a_baseline_only_deferred_gate_scenarios,
    a_claimed_no_diff_handoff_replays_a_baseline_only_deferred_gate,
    a_claimed_no_diff_replay_that_still_defers_is_refused,
    a_claimed_no_diff_replay_that_ran_no_tests_is_refused
);

crate::scenario_table!(
    a_claimed_no_diff_handoff_without_a_deferral_replays_nothing_scenarios,
    a_claimed_no_diff_handoff_without_a_deferral_replays_nothing,
    a_replica_lacking_the_owners_baseline_gate_accepts_and_replays_its_deferral
);

crate::scenario_table!(
    a_deferral_of_a_replica_only_gate_is_refused_naming_the_owners_gates_scenarios,
    a_deferral_of_a_replica_only_gate_is_refused_naming_the_owners_gates,
    a_known_red_handoff_the_owners_base_run_contradicts_ends_the_claim
);

#[cfg(unix)]
crate::scenario_table!(
    a_claimed_already_resolved_base_replays_its_named_tests_and_completes_without_a_pr_scenarios,
    a_claimed_already_resolved_base_replays_its_named_tests_and_completes_without_a_pr,
    a_failing_already_resolved_replay_blocks_the_task_with_its_output
);
