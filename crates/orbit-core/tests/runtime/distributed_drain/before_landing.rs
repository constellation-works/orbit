//! A claimed leaf from an owner with `review.before_landing` on reviews its
//! open pull request on the leaf, after `pr_open` and before `handoff`, and
//! the owner accepts — and so can land — only a handoff whose before-landing
//! review settled the exact head handed off [ORB-14849].

use super::claimed_review::{REVIEW_CREW, ReviewedLeaf, revision};
use super::*;

use orbit_core::application::distributed::ClaimedLeafStage;
use orbit_types::workflow::handoff::HandoffReviewEvidence;
use orbit_types::workflow::{ReviewAdmission, ReviewVerdict};

/// The current claimed implementation reaches the owner's review manifest
/// before handoff acceptance, even when owner task state is empty or stale.
fn a_claimed_before_landing_review_reads_the_current_implementers_summary() {
    let current = "Outcome: success\nImplemented and validated this candidate.";
    let stale = "Outcome: failed\nAn earlier attempt failed.";
    for (owner_summary, implementation, expected) in [
        ("", json!({"execution_summary": current}), current),
        (
            stale,
            json!({"execution_summary": format!("  {current}\n"), "summary": "short account"}),
            current,
        ),
        (stale, json!({"summary": current}), current),
        ("", json!({}), ""),
        (stale, json!({"execution_summary": "  "}), stale),
    ] {
        let mut leaf = ReviewedLeaf::admit_launched(
            &format!(
                "[review]\nbefore_landing = true\n\n[operation]\nreview_crew = \"{REVIEW_CREW}\"\n"
            ),
            "",
            |pair, drain| {
                // Only an earlier attempt can have written this owner
                // summary: seed it before the new execution claim exists.
                pair.wire
                    .owner
                    .update_task_with_identity(
                        &pair.tasks[0],
                        orbit_core::application::task::TaskUpdateParams {
                            execution_summary: Some(owner_summary.into()),
                            ..Default::default()
                        },
                        Some("codex".into()),
                        None,
                    )
                    .unwrap();
                pair.launched_leaf(drain, 1, std::process::id())
            },
        );
        // Exercise the real compound-step checkpoint boundary, which puts
        // implement_one in the run state before review admission.
        leaf.bound
            .checkpoint_step(
                &leaf.leaf,
                0,
                "implement_bundle",
                &json!({}),
                &BTreeMap::from([("implement_one".into(), implementation)]),
            )
            .unwrap();
        leaf.gate_input["before_landing"] = json!(true);
        let admitted = leaf.admit_review();
        assert_eq!(admitted["applies"], true, "{admitted}");
        let manifest: orbit_types::workflow::ReviewManifest = serde_json::from_slice(
            &leaf
                .owner_artifact(orbit_types::workflow::REVIEW_MANIFEST_ARTIFACT)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(manifest.implementer_summaries[&leaf.task], expected);
        assert_eq!(
            leaf.pair
                .wire
                .owner
                .get_task(&leaf.task)
                .unwrap()
                .execution_summary,
            owner_summary,
            "admission carries the current account without changing owner task state",
        );
    }
}

/// The claim captures the owner's before-landing review. On the leaf the
/// pre-push gate does not apply and the before-landing gate does; its
/// reviewer's fix is the reviewed head. The owner refuses a handoff without
/// that review, with it filed as a before-PR review, or for a head pushed
/// after it settled, and accepts the reviewed head as before-landing
/// evidence.
fn a_claimed_pr_lands_only_at_the_head_its_before_landing_review_settled() {
    let mut leaf = ReviewedLeaf::admit_before_landing();
    let pulls = leaf.pair.wire.calls("orbit.task.pull");
    assert_eq!(pulls[0]["review_gate"], true, "{}", pulls[0]);
    assert_eq!(pulls[0]["ship"]["before_landing"], true, "{}", pulls[0]);
    assert_eq!(pulls[0]["ship"]["before_pr"], false, "{}", pulls[0]);
    assert_eq!(pulls[0]["ship"]["review"]["crew"], "sol");

    let input = leaf
        .pair
        .follower_jobs
        .get_job_run(&leaf.leaf)
        .unwrap()
        .unwrap()
        .input
        .unwrap();
    let admission = ReviewAdmission::from_run_input(&input)
        .unwrap()
        .expect("the leaf carries the claim's review admission");
    assert!(admission.gates_landing() && !admission.gates_pr());

    let pre_push = leaf.admit_review();
    assert_eq!(pre_push["applies"], false, "{pre_push}");
    assert_eq!(pre_push["reason"], "review_before_landing");

    leaf.gate_input["before_landing"] = json!(true);
    let admitted = leaf.admit_review();
    assert_eq!(admitted["applies"], true, "{admitted}");
    let attempt_id = admitted["attempt_id"].as_str().unwrap().to_string();
    leaf.reviewer_reports(&attempt_id, ReviewVerdict::AcceptWithFixes, true);
    let settled = leaf.settle().expect("an accepted review passes");
    assert_eq!(settled["reviewer_fixed"], true, "{settled}");
    let reviewed = revision(&leaf.pair.follower_repo, "HEAD");
    assert_eq!(settled["reviewed_head_sha"], reviewed.commit.as_str());
    let evidence: HandoffReviewEvidence =
        serde_json::from_value(settled["handoff_evidence"].clone()).expect("handoff evidence");

    let refused = leaf
        .owner_accepts_review(HandoffReview::not_required(), &reviewed)
        .expect_err("an unreviewed handoff is refused");
    assert!(
        refused.to_string().contains("review_evidence_missing"),
        "{refused}"
    );
    let refused = leaf
        .owner_accepts(evidence.clone(), &reviewed)
        .expect_err("a before-PR review cannot stand in for the captured timing");
    assert!(
        refused.to_string().contains("review_evidence_missing"),
        "{refused}"
    );

    // A head pushed after the review settled was never reviewed.
    std::fs::write(
        leaf.pair.follower_repo.join("src/f0.rs"),
        "fn work() {}\n// pushed after review\n",
    )
    .unwrap();
    git(&leaf.pair.follower_repo, &["commit", "-q", "-am", "Later"]);
    let later = revision(&leaf.pair.follower_repo, "HEAD");
    let landing_review = |evidence: &HandoffReviewEvidence| HandoffReview {
        policy: ReviewTiming::BeforeLanding,
        disposition: HandoffReviewDisposition::BeforeLanding(Box::new(evidence.clone())),
    };
    let refused = leaf
        .owner_accepts_review(landing_review(&evidence), &later)
        .expect_err("an unreviewed head is refused");
    assert!(
        refused.to_string().contains("reviewed_head_mismatch"),
        "{refused}"
    );
    assert_eq!(leaf.pair.owner_status(&leaf.task), "in-progress");

    leaf.owner_accepts_review(landing_review(&evidence), &reviewed)
        .expect("the owner accepts the before-landing review of the handed-off head");
    assert_eq!(leaf.pair.owner_status(&leaf.task), "review");
}

/// [ORB-15194] A claimed leaf that has reached its before-landing review is
/// listed as reviewing rather than implementing, and its drain admits a
/// replacement beside it even though the leaf still holds its slot.
fn a_reviewing_leaf_lets_its_full_drain_admit_a_replacement() {
    let pair = Pair::with_owner_config(
        &format!(
            "[review]\nbefore_landing = true\n\n[operation]\nreview_crew = \"{REVIEW_CREW}\"\n"
        ),
        &[None, None],
    );
    let drain = pair.run_drain();
    let leaf = pair.running_leaf(&drain, 1);
    let stages = || {
        pair.follower
            .pull_drain_claimed_leaves(&drain)
            .expect("claimed leaves")
            .into_iter()
            .map(|leaf| (leaf.leaf_run_id, leaf.stage))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        stages(),
        vec![(leaf.clone(), ClaimedLeafStage::Implementing)]
    );
    let full = pair.pass_with(&drain, 1);
    assert_eq!(full["admitted"], 0, "{full}");
    assert_eq!(pair.leaf_runs(), vec![leaf.clone()], "the drain is full");

    // The leaf's `landing_review_gate_admit` step checkpoints that its
    // before-landing review applies.
    let mut state = pair
        .follower
        .read_run_state(&leaf)
        .unwrap()
        .expect("leaf state");
    state.record_pipeline_output(
        "landing_review_gate_admit",
        crate::review_admission_shape::admitted_review("landing-attempt", "lineage", "HEAD"),
    );
    pair.follower.write_run_state(&leaf, &state).unwrap();
    assert_eq!(stages(), vec![(leaf.clone(), ClaimedLeafStage::Reviewing)]);

    let replacement = pair.queued_leaf(&drain, 1);
    assert_ne!(replacement, leaf);
    let occupancy = pair.follower_jobs.drain_leaf_occupancy().unwrap();
    assert_eq!(
        (occupancy.occupied, occupancy.reviewing),
        (2, 1),
        "the reviewing leaf keeps its slot beside its replacement"
    );
}

/// [ORB-15192] An owner whose own deliveries do not review before landing
/// still captures before-landing review, with its review crew, for the claims
/// of a machine `review.before_landing_hosts` lists. The probe answers per
/// caller machine, so any other label is offered no review, and the owner's
/// own delivery captures none.
fn before_landing_hosts_capture_landing_review_only_for_listed_machines() {
    let leaf = ReviewedLeaf::admit_from(
        &format!(
            "[review]\nbefore_landing_hosts = [\"{FOLLOWER}\"]\n\n[operation]\nreview_crew = \"{REVIEW_CREW}\"\n"
        ),
        "",
    );
    let owner = &leaf.pair.wire.owner;

    let listed = probe_as(owner, FOLLOWER);
    assert_eq!(listed["admits"], true, "{listed}");
    assert_eq!(listed["ship"]["before_landing"], true, "{listed}");
    assert_eq!(listed["ship"]["before_pr"], false, "{listed}");
    assert_eq!(listed["ship"]["review"]["crew"], REVIEW_CREW, "{listed}");
    assert_eq!(listed["review"]["before_landing"]["enabled"], false);
    assert_eq!(
        listed["review"]["before_landing"]["hosts"],
        json!([FOLLOWER])
    );
    for other in ["hm_other", OWNER] {
        let probe = probe_as(owner, other);
        assert_ne!(probe["ship"]["before_landing"], true, "{other}: {probe}");
        assert!(probe["ship"]["review"].is_null(), "{other}: {probe}");
    }

    // The listed follower's pull carried the probed contract, and its
    // claimed leaf gates landing on a review.
    let pulls = leaf.pair.wire.calls("orbit.task.pull");
    assert_eq!(pulls[0]["ship"]["before_landing"], true, "{}", pulls[0]);
    let input = leaf
        .pair
        .follower_jobs
        .get_job_run(&leaf.leaf)
        .unwrap()
        .unwrap()
        .input
        .unwrap();
    let admission = ReviewAdmission::from_run_input(&input)
        .unwrap()
        .expect("the leaf carries the claim's review admission");
    assert!(admission.gates_landing() && !admission.gates_pr());

    // The owner's own PR delivery never reads the host list. Its run is only
    // submitted: the catalog job stands in for the shipped pipeline, and the
    // admission is captured at submission.
    let jobs = owner.paths().global_dir.join("resources/jobs");
    std::fs::create_dir_all(&jobs).unwrap();
    std::fs::write(
        jobs.join("task_pr_pipeline.yaml"),
        json!({
            "schemaVersion": 2, "kind": "Job", "metadata": {"name": "task_pr_pipeline"},
            "spec": {"state": "enabled", "kind": "workflow", "steps": [{
                "id": "review_gate_admit",
                "spec": {"type": "deterministic", "action": "review_gate_admit", "config": {}},
            }]},
        })
        .to_string(),
    )
    .unwrap();
    orbit_core::test_support::install_substitute_pipeline_worker(["sh", "-c", "exit 0"]);
    let task = backlog_task(owner, &leaf.pair.owner_repo, "src/own.rs", None);
    let own = owner
        .submit_pipeline_run(
            "task_pr_pipeline",
            json!({"task_ids": [task]}),
            None,
            Some("test"),
        )
        .expect("the owner submits its own delivery");
    let input = owner
        .get_job_run(&own.run_id)
        .unwrap()
        .unwrap()
        .input
        .unwrap();
    let admission = ReviewAdmission::from_run_input(&input)
        .unwrap()
        .expect("the owner's delivery captures a review admission");
    assert_eq!(admission.timing, ReviewTiming::None, "{input}");
}

/// [ORB-15195] An owner whose `operation.review_crew` is a pool resolves it
/// to one member when it offers a claim, so the probe, the pull and the
/// claimed leaf's admission each carry exactly one reviewer crew in the
/// unchanged wire contract, and the pull is admitted under this build's
/// protocol fingerprint.
fn a_review_pool_owner_captures_one_reviewer_crew_per_claim() {
    const POOL: [&str; 2] = ["grok", "sol"];
    let leaf = ReviewedLeaf::admit_from(
        "[review]\nbefore_landing = true\n\n[operation]\nreview_crew = [\"sol\", \"grok\"]\n",
        "",
    );
    let owner = &leaf.pair.wire.owner;

    for _ in 0..4 {
        let probe = probe_as(owner, FOLLOWER);
        assert_eq!(probe["admits"], true, "{probe}");
        assert_eq!(
            probe["protocol_fingerprint"],
            orbit_store::contracts::distributed_drain_protocol_fingerprint(),
            "{probe}"
        );
        let crew = probe["ship"]["review"]["crew"].as_str().unwrap_or_default();
        assert!(POOL.contains(&crew), "one pool member is offered: {probe}");
    }

    let pulls = leaf.pair.wire.calls("orbit.task.pull");
    let pulled = pulls[0]["ship"]["review"]["crew"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(POOL.contains(&pulled.as_str()), "{}", pulls[0]);
    let input = leaf
        .pair
        .follower_jobs
        .get_job_run(&leaf.leaf)
        .unwrap()
        .unwrap()
        .input
        .unwrap();
    let admission = ReviewAdmission::from_run_input(&input)
        .unwrap()
        .expect("the leaf carries the claim's review admission");
    assert!(admission.gates_landing(), "{input}");
    assert_eq!(admission.crew.as_deref(), Some(pulled.as_str()), "{input}");
    assert!(admission.crew_pool.is_empty(), "{input}");
}

/// The owner's probe as `machine`'s trusted SSH session asks it.
fn probe_as(owner: &OrbitRuntime, machine: &str) -> Value {
    owner
        .run_tool_with_context_and_role(
            "orbit.drain.probe",
            json!({
                "caller_version": orbit_core::application::distributed::owner_binary_version(),
                "caller_schema": orbit_store::contracts::DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA,
            }),
            Role::Admin,
            ToolContext {
                session_context: ToolSessionContext {
                    caller_machine_id: Some(machine.to_string()),
                    process_machine_id: Some(OWNER.to_string()),
                    transport: Some(McpTransport::SshMcp),
                    effective_capabilities: BTreeSet::from([McpCapability::Agent]),
                    ..ToolSessionContext::default()
                },
                ..ToolContext::default()
            },
        )
        .expect("probe")
}

/// [ORB-15622] A claimed leaf's before-landing review settles a baseline
/// claim against the owner's `review.baseline_commands`, which its claim
/// captured, not against the replica's own list: the owner's check is rerun
/// on the base and confirms the red base, and a check only the replica lists
/// is refused as one the host does not rerun.
fn a_claimed_before_landing_review_confirms_baselines_against_the_owners_commands() {
    // Fails to build wherever it runs, base and candidate alike, so the
    // failure is held for the red base rather than counted known red.
    let scripts = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let command = |name: &str| {
        let script = scripts.path().join(name);
        std::fs::write(
            &script,
            "echo 'error: suite::broken does not build'\necho '  --> src/lib.rs:1:1'\nexit 1\n",
        )
        .unwrap();
        format!("sh '{}'", script.display())
    };
    let (owner_check, replica_check) = (command("owner-check.sh"), command("replica-check.sh"));
    let owner_config = format!(
        "[review]\nbefore_landing = true\nbaseline_commands = [\"{owner_check}\"]\n\n\
         [operation]\nreview_crew = \"{REVIEW_CREW}\"\n"
    );
    let replica_config = format!("[review]\nbaseline_commands = [\"{replica_check}\"]\n");
    let settle_claim = |claimed: &str| {
        let mut leaf = ReviewedLeaf::admit_from(&owner_config, &replica_config);
        leaf.gate_input["before_landing"] = json!(true);
        let admitted = leaf.admit_review();
        assert_eq!(admitted["applies"], true, "{admitted}");
        let report: orbit_types::workflow::ReviewReport = serde_json::from_value(json!({
            "schema_version": 1,
            "attempt_id": admitted["attempt_id"],
            "verdict": "reject",
            "summary": "The candidate is sound; the check fails on the base too.",
            "findings": [],
            "validation": [{
                "id": "V1", "command": claimed, "outcome": "failed", "role": "required",
                "sources": ["src/lib.rs"],
                "baseline": {
                    "base_commit": admitted["base_sha"], "outcome": "failed",
                    "failures": ["suite::broken"],
                },
            }],
            "escalation": "The required check fails on the pinned base.",
        }))
        .unwrap();
        leaf.put_report(&report);
        let failure = leaf
            .settle()
            .expect_err("a failed required check never passes");
        let certificate: orbit_types::workflow::ReviewCertificate = serde_json::from_slice(
            &leaf
                .owner_artifact(orbit_types::workflow::REVIEW_GATE_ARTIFACT)
                .expect("the certificate is on the owner's task"),
        )
        .unwrap();
        (failure, certificate)
    };

    let (failure, certificate) = settle_claim(&owner_check);
    assert!(
        orbit_types::workflow::is_baseline_red_failure(None, Some(&failure)),
        "the owner's check confirms the red base: {failure}"
    );
    assert_eq!(certificate.baseline_red.len(), 1, "{failure}");
    assert_eq!(certificate.baseline_red[0].command, owner_check);

    let (failure, certificate) = settle_claim(&replica_check);
    assert!(
        !orbit_types::workflow::is_baseline_red_failure(None, Some(&failure)),
        "{failure}"
    );
    assert!(certificate.baseline_red.is_empty());
    assert_eq!(certificate.verdict, ReviewVerdict::Incomplete);
    assert!(
        certificate.escalation.as_deref().is_some_and(|reason| {
            reason.contains("baseline_claim_refused") && reason.contains("this check is neither")
        }),
        "{:?}",
        certificate.escalation
    );
}

// Related boundary scenarios share the isolated table fixture.
crate::scenario_table!(
    a_claimed_before_landing_review_reads_the_current_implementers_summary_scenarios,
    a_claimed_before_landing_review_reads_the_current_implementers_summary,
    a_claimed_pr_lands_only_at_the_head_its_before_landing_review_settled,
    a_reviewing_leaf_lets_its_full_drain_admit_a_replacement,
    before_landing_hosts_capture_landing_review_only_for_listed_machines,
    a_review_pool_owner_captures_one_reviewer_crew_per_claim,
    a_claimed_before_landing_review_confirms_baselines_against_the_owners_commands
);
