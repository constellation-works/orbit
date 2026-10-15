//! Authenticated source continuation through the public landing and leaf contracts.

use super::*;

/// Source recovery shares the public claim transaction and pull machinery.
/// The fixture stands in for the owner observer, never for admission itself.
pub(super) fn evidenced_source_stop_is_one_authenticated_claim_and_keeps_selectors() {
    use super::claimed_review::{REVIEW_CREW, ReviewedLeaf, revision};
    use orbit_store::TaskCommitBoundary;
    use orbit_store::maintenance::task_registry::{TaskRegistryStore, task_registry_path};
    use orbit_types::workflow::ReviewVerdict;
    use orbit_types::workflow::handoff::{
        HandoffReviewEvidence, LandingCheckRecovery, LandingFailedCheck, LandingSourceCause,
        LandingSourceRepair,
    };

    for mode in [
        "repair",
        "untrusted_input",
        "no_authority",
        "foreign_task",
        "foreign_claim",
        "unresolved_intent",
        "protected",
        "missing_job",
        "native_before_pr_hold",
        "native_failed_review",
    ] {
        tracing::info!(mode, "source landing claim admission");
        let mut leaf = ReviewedLeaf::admit_launched(
            &format!(
                "[review]\nbefore_pr = {}\nbefore_landing = {}\n\n[operation]\nreview_crew = \"{REVIEW_CREW}\"\n{}",
                !matches!(mode, "repair" | "native_failed_review"),
                matches!(mode, "repair" | "native_failed_review"),
                if matches!(
                    mode,
                    "repair" | "native_before_pr_hold" | "native_failed_review"
                ) {
                    "\n[[review.host_evidence]]\nkind = \"native_os\"\nname = \"Native emitter execution\"\npaths = [\"src/**\"]\nos = \"macos\"\ncommand = \"scripts/check-ci-macos.sh\"\nartifact = \"native-emitter.json\"\nhosted_check = \"Native / macOS Sandbox\"\n"
                } else {
                    ""
                }
            ),
            "",
            |pair, drain| {
                let owner = &pair.wire.owner;
                let workspace = owner.workspace_id().unwrap();
                let jobs = orbit_store::compose::workspace_job_run_store(
                    owner.sqlite_store().unwrap(),
                    workspace.clone(),
                );
                // Host state is seeded through the same public capture boundary
                // operator submission calls, with a real active owner run.
                let grant = jobs
                    .insert_job_run(
                        "workspace_auto_pipeline",
                        1,
                        Utc::now(),
                        Some(json!({"auto_fix":true})),
                        None,
                    )
                    .unwrap();
                jobs.mark_job_run_running(&grant.run_id, Utc::now(), std::process::id())
                    .unwrap();
                if !matches!(mode, "no_authority" | "untrusted_input") {
                    TaskCommitBoundary::new(
                        owner.sqlite_store().unwrap(),
                        TaskRegistryStore::open(&task_registry_path(&owner.global_root())).unwrap(),
                        workspace,
                    )
                    .unwrap()
                    .capture_landing_repair_authority(&grant.run_id)
                    .unwrap();
                }
                pair.launched_leaf(drain, 1, std::process::id())
            },
        );
        leaf.gate_input["before_landing"] =
            json!(matches!(mode, "repair" | "native_failed_review"));
        // The inherited source lies outside the task's original selectors.
        let repo = &leaf.pair.follower_repo;
        let branch = git(repo, &["branch", "--show-current"]).trim().to_string();
        git(repo, &["checkout", "main"]);
        std::fs::write(repo.join("src/contract"), "broken\n").unwrap();
        git(repo, &["add", "src/contract"]);
        git(repo, &["commit", "-m", "shared source baseline"]);
        leaf.base = revision(repo, "main");
        git(repo, &["checkout", &branch]);
        git(repo, &["rebase", "main"]);
        let admitted = leaf.admit_review();
        leaf.reviewer_reports(
            admitted["attempt_id"].as_str().unwrap(),
            ReviewVerdict::AcceptWithFixes,
            true,
        );
        if mode == "native_failed_review" {
            use super::claimed_owed_evidence::{ci_fast, record, report};
            leaf.put_report(&report(
                &admitted,
                ReviewVerdict::Accept,
                Vec::new(),
                vec![
                    ci_fast(),
                    record(
                        "native",
                        "scripts/check-ci-macos.sh",
                        orbit_types::workflow::ValidationOutcome::Failed,
                    ),
                ],
                None,
            ));
        }
        if matches!(mode, "native_before_pr_hold" | "native_failed_review") {
            let refusal = leaf
                .settle()
                .expect_err("native execution is not a reviewer assertion");
            assert!(
                refusal.contains(if mode == "native_before_pr_hold" {
                    "review_awaiting_evidence"
                } else {
                    "review_gate_blocked"
                }),
                "{mode}: {refusal}"
            );
            continue;
        }
        let settled = leaf.settle().expect("independent review passes");
        let reviewed: HandoffReviewEvidence =
            serde_json::from_value(settled["handoff_evidence"].clone()).unwrap();
        let head = revision(&leaf.pair.follower_repo, "HEAD");
        accept_source_leaf(&leaf, reviewed.clone(), &head).unwrap();
        let owner = &leaf.pair.wire.owner;
        let (claim, _) = leaf.claim();
        let accepted = owner.accepted_task_handoff(&claim).unwrap();
        let observation = HandoffObservation {
            footprint_widening: vec![],
            candidate: accepted.handoff.candidate.clone(),
            required_commands: vec![],
            owner_completion_authority: None,
            review: None,
            known_red: vec![],
        };
        let operator =
            ClaimInvocation::trusted_operator(leaf.task.clone(), claim.clone(), "operator".into())
                .with_handoff_observation(observation.clone());
        orbit_core::test_support::install_substitute_pipeline_worker(["true".to_string()]);
        owner
            .approve_task_handoff(
                &operator,
                "source-approve",
                accepted.handoff_id.clone(),
                accepted.handoff.candidate.clone(),
                observation,
            )
            .unwrap();
        let native_context =
            RuntimeHost::handoff_landing_context(owner, &accepted.handoff_id).unwrap();
        if mode == "repair" {
            let certificate = native_context
                .owner_review_certificate
                .as_ref()
                .expect("owner accepted certificate");
            assert_eq!(certificate.owner_owed_evidence.len(), 1);
            assert_eq!(
                certificate.owner_owed_evidence[0].hosted_check.as_deref(),
                Some("Native / macOS Sandbox")
            );
        }
        owner
            .mutate_execution_claim(
                Some(&operator),
                "updated-source-base",
                &ClaimMutation::RecordLandingRecovery {
                    handoff_id: accepted.handoff_id.clone(),
                    recovery: LandingCheckRecovery {
                        updated: true,
                        source_signatures: vec![(
                            "Native".into(),
                            "macOS Sandbox".into(),
                            "source contract failed".into(),
                        )],
                        actions: vec!["one base update admitted".into()],
                        ..Default::default()
                    },
                },
            )
            .unwrap();
        let mut source = LandingSourceRepair {
            failed_head: head.commit.clone(),
            pull_request: 42,
            cause: LandingSourceCause::RepeatedInheritedSourceFailure,
            permitted_paths: vec!["src/contract".into()],
            checks: vec![LandingFailedCheck {
                run_id: 1,
                job_id: 2,
                workflow: "Native".into(),
                name: "macOS Sandbox".into(),
                failed_step: "Emit native execution".into(),
                signature: "source contract failed".into(),
                diagnostic: "error: src/contract: broken source".into(),
                owner_requirement: native_context
                    .owner_review_certificate
                    .as_ref()
                    .and_then(|certificate| certificate.owner_owed_evidence.first().cloned()),
            }],
        };
        if mode == "protected" {
            source.permitted_paths = vec![".github/workflows/ci.yml".into()];
        }
        if mode == "missing_job" {
            source.checks.clear();
        }
        let selectors = leaf.pair.owner_task(&leaf.task)["context_files"].clone();
        let auth = if mode == "foreign_task" {
            ClaimInvocation::trusted_operator("foreign".into(), claim.clone(), "operator".into())
        } else if mode == "foreign_claim" {
            ClaimInvocation::trusted_operator(
                leaf.task.clone(),
                "foreign".into(),
                "operator".into(),
            )
        } else {
            operator.clone()
        };
        if mode == "unresolved_intent" {
            owner
                .mutate_execution_claim(
                    Some(&operator),
                    "pending-merge",
                    &ClaimMutation::MergeIntent {
                        intent_id: "uncertain".into(),
                        resolved: false,
                        evidence: "reply was lost".into(),
                    },
                )
                .unwrap();
        }
        let auth = if mode == "untrusted_input" {
            auth
        } else {
            auth.with_source_repair(source.clone())
        };
        let request = ClaimMutation::StopSourceRepair {
            handoff_id: accepted.handoff_id.clone(),
            reason: "same inherited source failure after the one base update".into(),
        };
        let result = if mode == "repair" {
            let barrier = Arc::new(std::sync::Barrier::new(2));
            let calls: Vec<_> = (0..2)
                .map(|_| {
                    let (owner, auth, request, barrier) = (
                        owner.clone(),
                        auth.clone(),
                        request.clone(),
                        barrier.clone(),
                    );
                    std::thread::spawn(move || {
                        barrier.wait();
                        owner.mutate_execution_claim(Some(&auth), "source-stop", &request)
                    })
                })
                .collect();
            let mut result = None;
            for call in calls {
                result = Some(call.join().unwrap());
                assert!(
                    result.as_ref().unwrap().is_ok(),
                    "concurrent source admission must reconcile its durable receipt"
                );
            }
            result.unwrap()
        } else {
            owner.mutate_execution_claim(Some(&auth), "source-stop", &request)
        };
        if mode != "repair" {
            assert!(result.is_err(), "{mode}: {result:?}");
            assert_eq!(
                leaf.pair.owner_status(&leaf.task),
                "review",
                "refusal starts no repair: {mode}"
            );
            continue;
        }
        result.expect("captured source authority admits RepairPending");
        assert_eq!(leaf.pair.owner_status(&leaf.task), "in-progress");
        assert!(
            owner.land_handoff(&accepted.handoff_id).is_err(),
            "the old immutable handoff is revoked"
        );
        // Lost-reply replay is the same durable decision, not a second admission.
        owner
            .mutate_execution_claim(Some(&auth), "source-stop", &request)
            .unwrap();
        let live = leaf.pair.pass(&leaf.drain);
        assert_eq!(
            live["admitted"], 0,
            "a live worker cannot start another repair agent"
        );
        leaf.pair.advance(
            &leaf.leaf,
            LocalPullMutation::Settle(Box::new(ClaimMutation::AcceptHandoff(
                accepted.handoff.clone(),
            ))),
        );
        leaf.pair
            .follower_jobs
            .finalize_job_run(&leaf.leaf, JobRunState::Success, Utc::now(), None)
            .unwrap();
        leaf.pair.advance(&leaf.leaf, LocalPullMutation::Settled);
        let new_leaf = leaf.pair.launched_leaf(&leaf.drain, 1, std::process::id());
        let receipt = leaf.pair.admission(&new_leaf).receipt.unwrap();
        let repair = receipt.claim.unwrap();
        assert_eq!(repair.task_id, leaf.task);
        assert_eq!(
            repair.repair.as_ref().unwrap().source.as_ref(),
            Some(&source)
        );
        assert!(
            repair.footprint.contains(&"file:src/contract".to_string()),
            "explicit source scope is reserved"
        );
        assert_eq!(
            leaf.pair.owner_task(&leaf.task)["context_files"],
            selectors,
            "original selectors never silently widen"
        );
        let pass = leaf.pair.pass(&leaf.drain);
        assert!(pass["error"].is_null(), "{pass}");
        assert_eq!(
            leaf.pair.owner_claims().len(),
            2,
            "one original claim and exactly one repair"
        );
        let boundary = TaskCommitBoundary::new(
            owner.sqlite_store().unwrap(),
            TaskRegistryStore::open(&task_registry_path(&owner.global_root())).unwrap(),
            owner.workspace_id().unwrap(),
        )
        .unwrap();
        assert_eq!(
            boundary
                .execution_claim_state(&claim)
                .unwrap()
                .unwrap()
                .claim
                .phase,
            orbit_store::contracts::ExecutionClaimPhase::Revoked
        );
        assert!(
            boundary
                .landing_attempt(&accepted.handoff_id)
                .unwrap()
                .unwrap()
                .source_repair
                .is_some()
        );
        assert_eq!(
            boundary
                .execution_claim_state(&repair.claim_id)
                .unwrap()
                .unwrap()
                .claim
                .repair
                .as_ref()
                .unwrap()
                .stop_evidence,
            "same inherited source failure after the one base update"
        );
        let owner = owner.clone();
        finish_source_leaf_and_stop_second_attempt(leaf, &new_leaf, &accepted, &reviewed, &owner);
    }
}

/// The fresh claim restores the candidate, uses the same branch and lease,
/// commits source repair, reruns independent review, and cannot reuse an old certificate.
fn finish_source_leaf_and_stop_second_attempt(
    mut prior: super::claimed_review::ReviewedLeaf,
    new_leaf: &str,
    original: &AcceptedHandoff,
    old_review: &orbit_types::workflow::handoff::HandoffReviewEvidence,
    owner: &OrbitRuntime,
) {
    use super::claimed_owed_evidence::{ci_fast, publish_origin, report};
    use super::claimed_review::{ReviewedLeaf, ToOwner, revision};
    use orbit_types::workflow::ReviewVerdict;
    // The fixture's origin is local; all lease and publication commands are real.
    let (root, origin) = publish_origin(&prior);
    let named_origin = root.join("owner/repository.git");
    std::fs::create_dir_all(named_origin.parent().unwrap()).unwrap();
    std::fs::rename(&origin, &named_origin).unwrap();
    git(
        &prior.pair.follower_repo,
        &[
            "remote",
            "set-url",
            "origin",
            "https://github.com/owner/repository.git",
        ],
    );
    git(
        &prior.pair.follower_repo,
        &[
            "config",
            &format!("url.{}.insteadOf", named_origin.display()),
            "https://github.com/owner/repository.git",
        ],
    );
    git(
        &prior.pair.follower_repo,
        &["push", "origin", &original.handoff.candidate.source_branch],
    );
    let record = prior.pair.admission(new_leaf);
    let claim = record.receipt.as_ref().unwrap().claim.as_ref().unwrap();
    let bound = prior
        .pair
        .follower
        .clone()
        .with_worker_invocation(
            WorkerInvocation {
                owner_machine_id: OWNER.into(),
                owner_workspace_id: record.destination.owner_workspace_id.clone(),
                owner_destination: record.destination.selector.clone(),
                task_id: claim.task_id.clone(),
                claim_id: claim.claim_id.clone(),
                execution: claim.executed_on.clone(),
                bound_run_id: new_leaf.into(),
            },
            Arc::new(ToOwner(owner.clone())),
        )
        .unwrap();
    let worktree = engine_action(
        &bound,
        "worktree_setup",
        &json!({"job_run_id":new_leaf,"run_id":new_leaf,"task_ids":[prior.task],"base":"main","base_sync":"remote","landing_mode":"pr"}),
    );
    let input = prior
        .pair
        .follower_jobs
        .get_job_run(new_leaf)
        .unwrap()
        .unwrap()
        .input
        .unwrap();
    let resumed = engine_action(
        &bound,
        "candidate_resume",
        &json!({"job_run_id":new_leaf,"task_ids":[prior.task],"workspace_path":worktree["workspace_path"],"base_sha":worktree["base_sha"],"claimed":true,"claim_repair":input["claim_repair"]}),
    );
    assert_eq!(
        resumed["repair"]["source"]["permitted_paths"],
        json!(["src/contract"])
    );
    assert_eq!(
        resumed["repair"]["source"]["checks"][0]["owner_requirement"]["command"],
        "scripts/check-ci-macos.sh",
        "fresh leaf retains the captured native command"
    );
    assert_eq!(
        resumed["reused_head_sha"],
        original.handoff.candidate.candidate.commit
    );
    let checkout = PathBuf::from(worktree["workspace_path"].as_str().unwrap());
    assert_eq!(
        git(&checkout, &["branch", "--show-current"]).trim(),
        original.handoff.candidate.source_branch
    );
    std::fs::write(checkout.join("src/contract"), "fixed\n").unwrap();
    let committed = engine_action(
        &bound,
        "git_commit",
        &json!({"job_run_id":new_leaf,"run_id":new_leaf,"scope":"all","workspace_path":checkout,"base_ref":worktree["base_ref"],"base_sha":worktree["base_sha"],"implementation":{"status":"success","execution_summary":"Outcome: success\nRepaired the inherited source contract."}}),
    );
    assert_eq!(committed["committed"], true);
    prior.pair.follower_repo = checkout.clone();
    let mut fresh = ReviewedLeaf {
        pair: prior.pair,
        drain: prior.drain,
        leaf: new_leaf.into(),
        task: prior.task,
        bound,
        base: prior.base,
        gate_input: json!({"run_id":new_leaf,"job_run_id":new_leaf,"completed_task_ids":[claim.task_id],"workspace_path":checkout,"base":"main","base_sync":"remote","mode":"pr","skipped_no_diff_expected":false,"before_landing":true}),
    };
    let repaired_head = revision(&checkout, "HEAD");
    assert!(
        fresh
            .owner_accepts_review(
                orbit_types::workflow::handoff::HandoffReview {
                    policy: orbit_types::workflow::ReviewTiming::BeforeLanding,
                    disposition:
                        orbit_types::workflow::handoff::HandoffReviewDisposition::BeforeLanding(
                            Box::new(old_review.clone())
                        ),
                },
                &repaired_head
            )
            .is_err(),
        "old matching-task certification cannot certify a new claim/head"
    );
    let admitted = fresh.admit_review();
    fresh.put_report(&report(
        &admitted,
        ReviewVerdict::Accept,
        Vec::new(),
        vec![ci_fast()],
        None,
    ));
    let settled = fresh.settle().expect("fresh independent review");
    let evidence = serde_json::from_value(settled["handoff_evidence"].clone()).unwrap();
    engine_action(
        &fresh.bound,
        "git_push",
        &json!({"job_run_id":new_leaf,"completed_task_ids":[fresh.task],"workspace_path":checkout,"branch":original.handoff.candidate.source_branch,"base":"main","base_ref":worktree["base_ref"],"candidate_resume":resumed,"implementation":{"status":"success","execution_summary":"Outcome: success\nRepaired the inherited source contract."}}),
    );
    accept_source_leaf(&fresh, evidence, &repaired_head)
        .expect("a fresh reviewed handoff is accepted");
    let repaired = owner.accepted_task_handoff(&claim.claim_id).unwrap();
    let observation = HandoffObservation {
        footprint_widening: vec![],
        candidate: repaired.handoff.candidate.clone(),
        required_commands: vec![],
        owner_completion_authority: None,
        review: None,
        known_red: vec![],
    };
    let operator = ClaimInvocation::trusted_operator(
        fresh.task.clone(),
        claim.claim_id.clone(),
        "operator".into(),
    )
    .with_handoff_observation(observation.clone());
    owner
        .approve_task_handoff(
            &operator,
            "repaired-authority",
            repaired.handoff_id.clone(),
            repaired.handoff.candidate.clone(),
            observation,
        )
        .unwrap();
    let context = RuntimeHost::handoff_landing_context(owner, &repaired.handoff_id).unwrap();
    assert!(context.source_repair.is_some());
    assert_eq!(
        context.owed_hosted_checks,
        vec!["Native / macOS Sandbox"],
        "native failure identity stays owed on the new head even without a reviewer prompt"
    );
    owner
        .mutate_execution_claim(
            Some(&operator),
            "second-source-stop",
            &ClaimMutation::StopLanding {
                handoff_id: repaired.handoff_id.clone(),
                reason: "the repaired source still failed its protected native check".into(),
                repairable: true,
            },
        )
        .unwrap();
    assert_eq!(fresh.pair.owner_status(&fresh.task), "blocked");
    let comments = comments_of(&fresh.pair.owner_task(&fresh.task));
    assert!(
        comments.contains(&original.handoff.candidate.candidate.commit)
            && comments.contains(&repaired_head.commit)
            && comments.contains("same inherited source failure")
            && comments.contains("repaired source still failed"),
        "both attempts retained: {comments}"
    );
    assert_eq!(
        fresh.pair.owner_claims().len(),
        2,
        "watcher budget cannot mint more landing claim repairs"
    );
}

fn accept_source_leaf(
    leaf: &super::claimed_review::ReviewedLeaf,
    evidence: orbit_types::workflow::handoff::HandoffReviewEvidence,
    head: &orbit_types::workflow::automation::SourceRevision,
) -> Result<(), OrbitError> {
    use orbit_types::workflow::{
        ReviewTiming,
        handoff::{HandoffReview, HandoffReviewDisposition},
    };
    if leaf.gate_input["before_landing"] == true {
        leaf.owner_accepts_review(
            HandoffReview {
                policy: ReviewTiming::BeforeLanding,
                disposition: HandoffReviewDisposition::BeforeLanding(Box::new(evidence)),
            },
            head,
        )
    } else {
        leaf.owner_accepts(evidence, head)
    }
}
