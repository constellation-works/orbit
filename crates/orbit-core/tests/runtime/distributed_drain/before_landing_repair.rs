//! Captured owner review decisions and exact-head before-landing enforcement
//! across distributed landing-repair claims.

use super::claimed_review::{REVIEW_CREW, ReviewedLeaf, ToOwner, revision};
use super::*;
use orbit_engine::ReviewGate;
use orbit_types::tool::WorkerInvocation;
use orbit_types::workflow::handoff::HandoffReviewEvidence;
use orbit_types::workflow::{ReviewAdmission, ReviewVerdict};

/// Approve, dispatch and record a repairable landing stop on `leaf`'s accepted
/// handoff so the owner moves its claim to `repair_pending` and returns the
/// task to `in-progress`.
fn stop_handoff_for_repair(leaf: &ReviewedLeaf, delivered_candidate: &HandoffCandidate) {
    let owner = &leaf.pair.wire.owner;
    let (claim_id, _) = leaf.claim();
    let accepted = owner
        .accepted_task_handoff(&claim_id)
        .expect("accepted handoff");
    let handoff_id = accepted.handoff_id.clone();
    let local = leaf.pair.admission(&leaf.leaf);
    for mutation in [
        LocalPullMutation::Settle(Box::new(ClaimMutation::AcceptHandoff(accepted.handoff))),
        LocalPullMutation::Settled,
    ] {
        leaf.pair
            .follower_jobs
            .mutate_local_pull(&local.destination, &local.request.request_id, &mutation)
            .expect("mark first leaf settled on follower");
    }
    let landing =
        ClaimInvocation::trusted_operator(leaf.task.clone(), claim_id, "owner-landing".into())
            .with_handoff_observation(HandoffObservation {
                candidate: delivered_candidate.clone(),
                footprint_widening: vec![],
                required_commands: vec![],
                owner_completion_authority: None,
                review: None,
                known_red: vec![],
                light_delivery: Vec::new(),
            });
    for (id, mutation) in [
        (
            "approve-landing",
            ClaimMutation::ApproveHandoff {
                handoff_id: handoff_id.clone(),
                candidate: delivered_candidate.clone(),
            },
        ),
        (
            "dispatch-landing",
            ClaimMutation::DispatchLanding {
                handoff_id: handoff_id.clone(),
                job_run_id: None,
            },
        ),
    ] {
        owner
            .mutate_execution_claim(Some(&landing), id, &mutation)
            .expect("landing transition");
    }
    ReviewGate::record_handoff_landing(
        &orbit_core::test_support::EngineHost::new(owner),
        &orbit_engine::HandoffLandingUpdate {
            handoff_id,
            step: orbit_engine::HandoffLandingStep::Stop { repairable: true },
            observed: None,
            evidence: "pull request conflicts with its base".into(),
        },
    )
    .expect("repairable stop");
}

/// Bind `leaf` on `pair.follower` for review-gate execution against
/// `pair.wire.owner`.
fn bind_claimed_leaf(pair: &Pair, leaf: &str) -> (OrbitRuntime, String, Value) {
    let record = pair.admission(leaf);
    let claim = record.receipt.as_ref().unwrap().claim.clone().unwrap();
    let bound = pair
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
                bound_run_id: leaf.to_string(),
            },
            Arc::new(ToOwner(pair.wire.owner.clone())),
        )
        .unwrap();
    let gate_input = json!({
        "run_id": leaf,
        "job_run_id": leaf,
        "completed_task_ids": [claim.task_id],
        "workspace_path": pair.follower_repo,
        "base": "main",
        "base_sync": "local",
        "mode": "pr",
        "skipped_no_diff_expected": false,
    });
    (bound, claim.claim_id, gate_input)
}

/// Overwrite `run.input` directly in the follower store to simulate a legacy
/// claimed leaf persisted before owner-off admissions wrote `input.review`, or
/// a submitted pull supervisor carrying its local review config.
fn replace_job_run_input(follower: &OrbitRuntime, run_id: &str, input: Value) {
    let workspace_id = follower.workspace_id().unwrap();
    follower
        .sqlite_store()
        .unwrap()
        .write(|tx| {
            let runs = tx.job_run();
            let mut run = runs.get(&workspace_id, run_id)?.expect("job run exists");
            run.input = Some(input);
            runs.put(&workspace_id, &run, None)
        })
        .unwrap();
}

/// When the owner explicitly disables before-landing review, a claimed landing
/// repair reports that captured owner decision distinctly from the follower
/// pull supervisor's local before-landing config, and legacy claims without
/// `input.review` still resolve as owner-off without inferring review from
/// follower settings.
fn claimed_landing_repair_reports_owner_off_decision_despite_follower_before_landing_config() {
    let follower_config = format!(
        "[review]\ntiming = \"before_landing\"\n\n[workflow.purpose_crews]\nreview = \"{REVIEW_CREW}\"\n"
    );
    let leaf = ReviewedLeaf::admit_launched(
        "[review]\ntiming = \"off\"\n",
        &follower_config,
        |pair, drain| {
            let local_supervisor_review = json!({
                "contract_version": orbit_types::workflow::REVIEW_CONTRACT_VERSION,
                "policy_version": 1,
                "timing": "before-landing",
                "timing_source": "workspace",
                "crew": REVIEW_CREW,
                "crew_source": "workspace",
                "budget": orbit_types::workflow::ReviewBudget::default(),
                "required_validation_commands": [],
                "captured_at": Utc::now(),
            });
            replace_job_run_input(
                &pair.follower,
                drain,
                json!({"review": local_supervisor_review}),
            );
            pair.launched_leaf(drain, 1, std::process::id())
        },
    );
    assert_eq!(
        leaf.pair.follower.run_review_layer(&leaf.drain).unwrap(),
        json!({
            "timing": "before-landing",
            "source": "workspace",
            "scope": "supervisor_local",
        }),
        "pull supervisor marks its local snapshot distinct from claim policy"
    );

    let first_head = revision(&leaf.pair.follower_repo, "HEAD");
    leaf.owner_accepts_review(HandoffReview::not_required(), &first_head)
        .expect("owner-off initial handoff accepted");
    let (first_claim_id, _) = leaf.claim();
    let first_candidate = leaf
        .pair
        .wire
        .owner
        .accepted_task_handoff(&first_claim_id)
        .unwrap()
        .handoff
        .candidate;
    stop_handoff_for_repair(&leaf, &first_candidate);
    assert_eq!(
        leaf.pair.owner_claim(&first_claim_id)["claim"]["phase"],
        "repair_pending"
    );

    leaf.pair
        .follower_jobs
        .finalize_job_run(&leaf.leaf, JobRunState::Success, Utc::now(), None)
        .unwrap();
    let repair_leaf = leaf.pair.launched_leaf(&leaf.drain, 1, std::process::id());
    assert_ne!(repair_leaf, leaf.leaf);
    let (bound, repair_claim_id, mut gate_input) = bind_claimed_leaf(&leaf.pair, &repair_leaf);

    // Owner console and history capture the owner-off decision on the repair claim.
    let console = leaf.pair.wire.owner.distributed_claim_console().unwrap();
    let console_repair = console["claims"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["claim_id"] == repair_claim_id.as_str())
        .expect("repair claim in owner console");
    assert_eq!(
        console_repair["repair"]["repairs_claim_id"],
        first_claim_id.as_str()
    );
    assert_eq!(console_repair["repair"]["owner_review_timing"], "none");
    let repair_pulled_note = leaf
        .pair
        .wire
        .owner
        .get_task_history(&leaf.task)
        .unwrap()
        .into_iter()
        .rev()
        .find(|entry| entry.event == "pulled_by")
        .and_then(|entry| entry.note)
        .and_then(|note| serde_json::from_str::<Value>(&note).ok())
        .expect("repair pulled_by note");
    assert_eq!(repair_pulled_note["claim_id"], repair_claim_id.as_str());
    assert_eq!(repair_pulled_note["review_timing"], "none");

    // Follower leaf surfaces report explicit owner-off (`none` from `claim`).
    let repair_input = leaf
        .pair
        .follower_jobs
        .get_job_run(&repair_leaf)
        .unwrap()
        .unwrap()
        .input
        .unwrap();
    assert_eq!(
        repair_input["claim_repair"]["owner_review_timing"], "none",
        "{repair_input}"
    );
    let admission = ReviewAdmission::from_run_input(&repair_input)
        .unwrap()
        .expect("owner-off repair leaf carries explicit review admission");
    assert_eq!(admission.timing, ReviewTiming::None);
    assert_eq!(admission.timing_source, "claim");
    assert_eq!(
        leaf.pair.follower.run_review_layer(&repair_leaf).unwrap(),
        json!({"timing": "none", "source": "claim"})
    );
    let pull_claim = leaf
        .pair
        .follower
        .pull_leaf_claim(&repair_leaf)
        .unwrap()
        .expect("pull_claim for repair leaf");
    assert_eq!(pull_claim.owner_review_timing, ReviewTiming::None);
    assert_eq!(
        pull_claim.repairs_claim_id.as_deref(),
        Some(first_claim_id.as_str())
    );
    assert!(
        pull_claim.describe().contains("owner_review=none")
            && pull_claim
                .describe()
                .contains(&format!("repairs {first_claim_id}")),
        "{}",
        pull_claim.describe()
    );
    let claimed_leaves = leaf
        .pair
        .follower
        .pull_drain_claimed_leaves(&leaf.drain)
        .unwrap();
    assert!(
        claimed_leaves.iter().any(|entry| {
            entry.leaf_run_id == repair_leaf
                && entry.owner_review_timing == ReviewTiming::None
                && entry.describe().contains("owner_review=none")
        }),
        "{claimed_leaves:#?}"
    );

    // Review gates report owner-off rather than `review_admission_missing`.
    let before_pr_gate = orbit_core::test_support::EngineHost::new(&bound)
        .run_deterministic(
            "review_gate_admit",
            &json!({}),
            &gate_input,
            ToolContext::default(),
        )
        .unwrap();
    assert_eq!(before_pr_gate["applies"], false, "{before_pr_gate}");
    assert_eq!(before_pr_gate["reason"], "review_before_pr_off");
    assert_eq!(before_pr_gate["timing"], "none");
    assert_eq!(before_pr_gate["timing_source"], "claim");

    gate_input["before_landing"] = json!(true);
    let landing_gate = orbit_core::test_support::EngineHost::new(&bound)
        .run_deterministic(
            "review_gate_admit",
            &json!({}),
            &gate_input,
            ToolContext::default(),
        )
        .unwrap();
    assert_eq!(landing_gate["applies"], false, "{landing_gate}");
    assert_eq!(landing_gate["reason"], "review_before_landing_off");
    assert_eq!(landing_gate["timing"], "none");
    assert_eq!(landing_gate["timing_source"], "claim");

    // Legacy owner-off claim without persisted `input.review` stays supported
    // and never infers before-landing review from follower config.
    let mut legacy_input = repair_input.clone();
    legacy_input.as_object_mut().unwrap().remove("review");
    replace_job_run_input(&leaf.pair.follower, &repair_leaf, legacy_input);
    let legacy_gate = orbit_core::test_support::EngineHost::new(&bound)
        .run_deterministic(
            "review_gate_admit",
            &json!({}),
            &gate_input,
            ToolContext::default(),
        )
        .unwrap();
    assert_eq!(legacy_gate["applies"], false, "{legacy_gate}");
    assert_eq!(legacy_gate["reason"], "review_before_landing_off");
    assert_eq!(legacy_gate["timing"], "none");
    assert_eq!(legacy_gate["timing_source"], "claim");

    // Owner accepts the repaired handoff with no review required.
    std::fs::write(
        leaf.pair.follower_repo.join("src/f0.rs"),
        "fn work() {}\n// repaired without review\n",
    )
    .unwrap();
    git(&leaf.pair.follower_repo, &["commit", "-q", "-am", "Repair"]);
    let repaired_head = revision(&leaf.pair.follower_repo, "HEAD");
    let repair_leaf_view = ReviewedLeaf {
        pair: leaf.pair,
        drain: leaf.drain,
        leaf: repair_leaf,
        task: leaf.task,
        bound,
        base: leaf.base,
        gate_input,
    };
    repair_leaf_view
        .owner_accepts_review(HandoffReview::not_required(), &repaired_head)
        .expect("owner accepts owner-off repaired handoff");
    assert_eq!(
        repair_leaf_view.pair.owner_status(&repair_leaf_view.task),
        "review"
    );
}

/// When the owner requires before-landing review (including via
/// `before_landing_hosts`), a landing-repair claim retains that contract
/// through follower startup and gate admission even when follower config is
/// `off`, fails closed if its persisted admission is missing, and enforces
/// passing review evidence bound to the exact repaired head at handoff.
fn claimed_landing_repair_retains_before_landing_contract_and_enforces_exact_repaired_head() {
    let owner_config = format!(
        "[review]\nbefore_landing_hosts = [\"{FOLLOWER}\"]\n\n\
         [workflow.purpose_crews]\npre_landing_review = \"{REVIEW_CREW}\"\n"
    );
    let mut first = ReviewedLeaf::admit_from(&owner_config, "[review]\ntiming = \"off\"\n");
    first.gate_input["before_landing"] = json!(true);
    let first_admitted = first.admit_review();
    assert_eq!(first_admitted["applies"], true, "{first_admitted}");
    let first_attempt = first_admitted["attempt_id"].as_str().unwrap().to_string();
    first.reviewer_reports(&first_attempt, ReviewVerdict::AcceptWithFixes, true);
    let first_settled = first.settle().expect("first review settles");
    let first_head = revision(&first.pair.follower_repo, "HEAD");
    let first_evidence: HandoffReviewEvidence =
        serde_json::from_value(first_settled["handoff_evidence"].clone()).unwrap();
    let landing_review = |evidence: &HandoffReviewEvidence| HandoffReview {
        selective_receipt: None,
        policy: ReviewTiming::BeforeLanding,
        disposition: HandoffReviewDisposition::BeforeLanding(Box::new(evidence.clone())),
    };
    first
        .owner_accepts_review(landing_review(&first_evidence), &first_head)
        .expect("first reviewed handoff accepted");
    let (first_claim_id, _) = first.claim();
    let first_candidate = first
        .pair
        .wire
        .owner
        .accepted_task_handoff(&first_claim_id)
        .unwrap()
        .handoff
        .candidate;
    stop_handoff_for_repair(&first, &first_candidate);
    assert_eq!(
        first.pair.owner_claim(&first_claim_id)["claim"]["phase"],
        "repair_pending"
    );

    first
        .pair
        .follower_jobs
        .finalize_job_run(&first.leaf, JobRunState::Success, Utc::now(), None)
        .unwrap();
    let repair_leaf_id = first
        .pair
        .launched_leaf(&first.drain, 1, std::process::id());
    assert_ne!(repair_leaf_id, first.leaf);
    let (bound, repair_claim_id, gate_input) = bind_claimed_leaf(&first.pair, &repair_leaf_id);

    // Owner and follower retain the before-landing contract across the repair.
    let console = first.pair.wire.owner.distributed_claim_console().unwrap();
    let console_repair = console["claims"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["claim_id"] == repair_claim_id.as_str())
        .expect("repair claim in owner console");
    assert_eq!(
        console_repair["repair"]["owner_review_timing"],
        "before-landing"
    );
    let repair_input = first
        .pair
        .follower_jobs
        .get_job_run(&repair_leaf_id)
        .unwrap()
        .unwrap()
        .input
        .unwrap();
    assert_eq!(
        repair_input["claim_repair"]["owner_review_timing"],
        "before-landing"
    );
    let admission = ReviewAdmission::from_run_input(&repair_input)
        .unwrap()
        .expect("repair leaf retains review admission");
    assert!(admission.gates_landing() && !admission.gates_pr());
    assert_eq!(admission.timing_source, "claim");
    assert_eq!(admission.crew.as_deref(), Some(REVIEW_CREW));
    assert_eq!(
        first
            .pair
            .follower
            .run_review_layer(&repair_leaf_id)
            .unwrap(),
        json!({"timing": "before-landing", "source": "claim"})
    );
    let pull_claim = first
        .pair
        .follower
        .pull_leaf_claim(&repair_leaf_id)
        .unwrap()
        .expect("pull_claim for repair leaf");
    assert_eq!(pull_claim.owner_review_timing, ReviewTiming::BeforeLanding);
    assert_eq!(
        pull_claim.repairs_claim_id.as_deref(),
        Some(first_claim_id.as_str())
    );

    // Missing persisted review admission on a review-required repair leaf fails closed.
    let mut stripped = repair_input.clone();
    stripped.as_object_mut().unwrap().remove("review");
    replace_job_run_input(&first.pair.follower, &repair_leaf_id, stripped);
    let mut missing_input = gate_input.clone();
    missing_input["before_landing"] = json!(true);
    let refused = orbit_core::test_support::EngineHost::new(&bound)
        .run_deterministic(
            "review_gate_admit",
            &json!({}),
            &missing_input,
            ToolContext::default(),
        )
        .expect_err("required review admission missing must fail closed")
        .to_string();
    assert!(refused.contains("review_admission_missing"), "{refused}");
    replace_job_run_input(&first.pair.follower, &repair_leaf_id, repair_input);

    // Commit the repaired candidate, review it under the repair claim's lineage,
    // and verify exact-head enforcement at owner handoff.
    std::fs::write(
        first.pair.follower_repo.join("src/f0.rs"),
        "fn work() {}\n// repaired candidate\n",
    )
    .unwrap();
    git(
        &first.pair.follower_repo,
        &["commit", "-q", "-am", "Repair"],
    );
    let mut repair = ReviewedLeaf {
        pair: first.pair,
        drain: first.drain,
        leaf: repair_leaf_id,
        task: first.task,
        bound,
        base: first.base,
        gate_input,
    };
    let pre_push = repair.admit_review();
    assert_eq!(pre_push["applies"], false, "{pre_push}");
    assert_eq!(pre_push["reason"], "review_before_landing");

    repair.gate_input["before_landing"] = json!(true);
    let admitted = repair.admit_review();
    assert_eq!(admitted["applies"], true, "{admitted}");
    assert!(
        admitted["lineage_key"]
            .as_str()
            .unwrap()
            .ends_with(&format!("/{repair_claim_id}")),
        "repair review uses its own claim lineage: {admitted}"
    );
    let repair_attempt = admitted["attempt_id"].as_str().unwrap().to_string();
    repair.reviewer_reports(&repair_attempt, ReviewVerdict::AcceptWithFixes, true);
    let settled = repair.settle().expect("repair review settles");
    let repaired_head = revision(&repair.pair.follower_repo, "HEAD");
    assert_eq!(settled["reviewed_head_sha"], repaired_head.commit.as_str());
    let repaired_evidence: HandoffReviewEvidence =
        serde_json::from_value(settled["handoff_evidence"].clone()).unwrap();

    // 1. Missing review evidence is held.
    let err = repair
        .owner_accepts_review(HandoffReview::not_required(), &repaired_head)
        .expect_err("missing review evidence held")
        .to_string();
    assert!(err.contains("review_evidence_missing"), "{err}");
    assert_eq!(repair.pair.owner_status(&repair.task), "in-progress");

    // 2. Failed/rejected verdict is held.
    let mut rejected = repaired_evidence.clone();
    rejected.verdict = ReviewVerdict::Reject;
    let err = repair
        .owner_accepts_review(landing_review(&rejected), &repaired_head)
        .expect_err("rejected review evidence held")
        .to_string();
    assert!(err.contains("review_not_passed"), "{err}");
    assert_eq!(repair.pair.owner_status(&repair.task), "in-progress");

    // 3. Evidence bound to the superseded first head is held.
    let err = repair
        .owner_accepts_review(landing_review(&first_evidence), &repaired_head)
        .expect_err("superseded head evidence held")
        .to_string();
    assert!(err.contains("reviewed_head_mismatch"), "{err}");
    assert_eq!(repair.pair.owner_status(&repair.task), "in-progress");

    // 4. Evidence bound to a different post-review head is held.
    std::fs::write(
        repair.pair.follower_repo.join("src/f0.rs"),
        "fn work() {}\n// unreviewed post-repair commit\n",
    )
    .unwrap();
    git(
        &repair.pair.follower_repo,
        &["commit", "-q", "-am", "Unreviewed"],
    );
    let unreviewed_head = revision(&repair.pair.follower_repo, "HEAD");
    let err = repair
        .owner_accepts_review(landing_review(&repaired_evidence), &unreviewed_head)
        .expect_err("different repaired head held")
        .to_string();
    assert!(err.contains("reviewed_head_mismatch"), "{err}");
    assert_eq!(repair.pair.owner_status(&repair.task), "in-progress");

    // 5. Passing review evidence for the exact repaired head is accepted.
    repair
        .owner_accepts_review(landing_review(&repaired_evidence), &repaired_head)
        .expect("exact repaired head review evidence accepted");
    assert_eq!(repair.pair.owner_status(&repair.task), "review");
}

crate::scenario_table!(
    claimed_landing_repair_review_decisions,
    claimed_landing_repair_reports_owner_off_decision_despite_follower_before_landing_config,
    claimed_landing_repair_retains_before_landing_contract_and_enforces_exact_repaired_head
);
