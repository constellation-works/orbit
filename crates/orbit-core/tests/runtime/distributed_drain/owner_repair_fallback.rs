//! The owner's fallback for a repair its original executor never pulls
//! [ORB-17303].
//!
//! A follower delivers, the owner's landing stops on a repairable failure, and
//! the follower's drain never asks again. The owner's own `orbit run auto`
//! drain is the only thing still running: its classify pass names the
//! executor while the repair is reserved for it, and once the fallback window
//! has passed starts a one-pass, repair-only pull against itself, whose real
//! `pull_refill` pass takes the repair through the owner's admission.

use super::*;

/// [ORB-17303] A follower's stopped landing whose executor never pulls again
/// is not stranded: the owner's `orbit run auto` drain names that executor
/// while the repair is reserved for it, and once the fallback window has
/// passed starts one repair-only pull against itself, which takes the repair
/// as a pull would and revokes the superseded claim.
fn the_owner_drain_takes_a_repair_its_original_executor_never_pulls() {
    owner_fallback(false, false);
}

fn the_owner_repairs_beside_an_implementing_leaf_and_waits_when_full() {
    owner_fallback(true, false);
}

fn repair_and_backlog_share_the_owner_ceiling_with_other_pulls() {
    owner_fallback(true, true);
}

/// A live worker holds each unrelated delivery until the scenario ends.
fn hold_owner_leaf(owner: &OrbitRuntime, task: &str) -> Worker {
    let worker = Worker::spawn();
    let jobs = orbit_store::compose::workspace_job_run_store(
        owner.sqlite_store().unwrap(),
        owner.workspace_id().unwrap(),
    );
    let run = jobs
        .insert_job_run(
            "task_pr_pipeline",
            1,
            Utc::now(),
            Some(json!({"task_ids": [task]})),
            None,
        )
        .unwrap();
    owner
        .write_run_state(
            &run.run_id,
            &PipelineState::new(run.run_id.clone(), run.job_id, json!({})),
        )
        .unwrap();
    jobs.mark_job_run_running(&run.run_id, Utc::now(), worker.pid)
        .unwrap();
    worker
}

fn owner_fallback(busy: bool, with_backlog: bool) {
    let pair = Pair::new(1);
    let task = pair.tasks[0].clone();
    *pair.wire.accept_handoffs.lock().unwrap() = true;
    let follower_drain = pair.run_drain();
    let leaf = pair.running_leaf(&follower_drain, 1);
    let delivered = handoff(&pair.admission(&leaf));
    pair.leaf_hands_off(&leaf);
    let settled = pair.pass(&follower_drain);
    assert!(settled["error"].is_null(), "{settled}");
    let itself = to_itself(&pair.wire.owner);
    let owner = pair
        .wire
        .owner
        .clone()
        .with_drain_owner_transport(itself.clone());
    launchable_providers(&owner);
    let handoff_id = owner
        .accepted_task_handoff(&delivered.claim_id)
        .unwrap()
        .handoff_id;
    let landing = ClaimInvocation::trusted_operator(
        task.clone(),
        delivered.claim_id.clone(),
        "owner-landing".into(),
    )
    .with_handoff_observation(HandoffObservation {
        candidate: delivered.candidate.clone(),
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
                candidate: delivered.candidate.clone(),
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
            .unwrap();
    }
    RuntimeHost::record_handoff_landing(
        &owner,
        &orbit_engine::HandoffLandingUpdate {
            handoff_id,
            step: orbit_engine::HandoffLandingStep::Stop { repairable: true },
            observed: None,
            evidence: "candidate-caused CI failure".into(),
        },
    )
    .unwrap();
    assert_eq!(
        pair.owner_claim(&delivered.claim_id)["claim"]["phase"],
        "repair_pending"
    );
    assert_eq!(pair.owner_status(&task), "in-progress");

    // The owner's own drain is running; the follower never pulls again.
    let workers = LandingWorkers::new();
    let jobs = owner.global_root().join("resources/jobs");
    std::fs::create_dir_all(&jobs).unwrap();
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/jobs/workspace_pull_pipeline.yaml"),
        jobs.join("workspace_pull_pipeline.yaml"),
    )
    .unwrap();
    let owner_jobs = orbit_store::compose::workspace_job_run_store(
        owner.sqlite_store().unwrap(),
        owner.workspace_id().unwrap(),
    );
    let auto = owner_jobs
        .insert_job_run("workspace_auto_pipeline", 1, Utc::now(), None, None)
        .unwrap();
    owner
        .write_run_state(
            &auto.run_id,
            &PipelineState::new(auto.run_id.clone(), auto.job_id, json!({})),
        )
        .unwrap();
    let auto = auto.run_id;
    let mut held_workers = Vec::new();
    if busy {
        let unrelated = backlog_task(&owner, &pair.owner_repo, "src/unrelated.rs", None);
        owner
            .run_tool(
                "orbit.task.update",
                json!({"id": unrelated, "status": "in-progress", "model": "codex"}),
            )
            .unwrap();
        held_workers.push(hold_owner_leaf(&owner, &unrelated));
        assert_eq!(owner_jobs.drain_leaf_occupancy().unwrap().occupied, 1);
    }
    let auto_pass = |ceiling| {
        let wave = owner
            .run_deterministic(
                "classify_workspace_auto_tasks",
                &json!({}),
                &json!({"run_id": auto, "max_active_leaf_runs": ceiling}),
                ToolContext::default(),
            )
            .expect("classify");
        let pass = owner
            .read_run_state(&auto)
            .unwrap()
            .unwrap()
            .pull_drain
            .drain_last_pass
            .expect("last pass");
        let waiting = pass
            .deferred
            .iter()
            .find(|waiting| waiting.task_id == task)
            .cloned()
            .unwrap_or_else(|| panic!("the pass reports the pending repair: {pass:#?}"));
        (wave, waiting)
    };

    let (wave, waiting) = auto_pass(4);
    assert_eq!(
        waiting.reason.as_deref(),
        Some("repair_reserved"),
        "{waiting:?}"
    );
    let detail = waiting.detail.unwrap_or_default();
    assert!(
        detail.contains(FOLLOWER),
        "names the original executor: {detail}"
    );
    assert!(wave["owner_repair_pull"].is_null(), "{wave}");
    assert!(
        owner_jobs
            .list_job_runs("workspace_pull_pipeline")
            .unwrap()
            .is_empty(),
        "no repair pull inside the fallback window"
    );

    orbit_core::test_support::with_repair_owner_fallback_elapsed(|| {
        if busy {
            // A due repair starts nothing when the unrelated leaf fills the
            // entire owner ceiling; no retry delay or child is created.
            let (full, _) = auto_pass(1);
            assert_eq!(full["free_slots"], 0, "{full}");
            assert!(full["owner_repair_pull"].is_null(), "{full}");
            assert!(
                owner_jobs
                    .list_job_runs("workspace_pull_pipeline")
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(
                pair.owner_claim(&delivered.claim_id)["claim"]["phase"],
                "repair_pending"
            );
        }
        if with_backlog {
            for n in 0..3 {
                backlog_task(
                    &owner,
                    &pair.owner_repo,
                    &format!("src/backlog{n}.rs"),
                    None,
                );
            }
        }
        let (wave, waiting) = auto_pass(4);
        let pull = wave["owner_repair_pull"]
            .as_str()
            .unwrap_or_else(|| panic!("the window passed, so a repair pull starts: {waiting:?}"))
            .to_string();
        assert_eq!(
            waiting.reason.as_deref(),
            Some("repair_owner_fallback"),
            "{waiting:?}"
        );
        assert!(
            waiting
                .detail
                .as_deref()
                .unwrap_or_default()
                .contains(&pull),
            "{waiting:?}"
        );
        workers.own(&pull).expect("the repair pull's worker");
        if with_backlog {
            // Interleave the owner's selected backlog wave before the repair
            // pull allocates. One slot remains reserved for the repair.
            let selected = wave["loose_task_ids"].as_array().unwrap();
            assert_eq!(selected.len(), 2, "{wave}");
            for task in selected {
                held_workers.push(hold_owner_leaf(&owner, task.as_str().unwrap()));
            }
            assert_eq!(owner_jobs.drain_leaf_occupancy().unwrap().occupied, 3);
        } else {
            // A pull already taking it is not started twice.
            let (again, _) = auto_pass(4);
            assert!(again["owner_repair_pull"].is_null(), "{again}");
        }
        assert_eq!(
            owner_jobs
                .list_job_runs("workspace_pull_pipeline")
                .unwrap()
                .len(),
            1
        );

        if owner.read_run_state(&pull).unwrap().is_none() {
            owner
                .write_run_state(
                    &pull,
                    &PipelineState::new(pull.clone(), "workspace_pull_pipeline".into(), json!({})),
                )
                .unwrap();
        }
        let input = owner_jobs
            .get_job_run(&pull)
            .unwrap()
            .unwrap()
            .input
            .unwrap();
        // The pass stops after the leaf exists and the owner bound its claim.
        itself.lose_next_reply("orbit.drain.claim.bind");
        let pass = owner
            .run_deterministic(
                "pull_refill",
                &json!({}),
                &json!({
                    "run_id": pull,
                    "destination": input["destination"],
                    "window_expired": true,
                    "for_seconds": input["for_seconds"],
                    "max_active_leaf_runs": input["max_active_leaf_runs"],
                }),
                ToolContext::default(),
            )
            .expect("a pass reports its errors instead of failing the drain");
        assert!(error_of(&pass).contains("dropped"), "{pass}");
        assert_eq!(
            owner_jobs.drain_leaf_occupancy().unwrap().occupied,
            if with_backlog {
                4
            } else if busy {
                2
            } else {
                1
            },
            "the repair occupies one shared slot beside the held deliveries"
        );
        if with_backlog {
            // Another pull races for the remaining backlog after the repair
            // bound its leaf: the same atomic workspace ceiling refuses it.
            let competitor = owner_jobs
                .insert_job_run("workspace_pull_pipeline", 1, Utc::now(), None, None)
                .unwrap();
            owner
                .write_run_state(
                    &competitor.run_id,
                    &PipelineState::new(competitor.run_id.clone(), competitor.job_id, json!({})),
                )
                .unwrap();
            let claims_before = pair.owner_claims().len();
            let full = owner
                .run_deterministic(
                    "pull_refill",
                    &json!({}),
                    &json!({"run_id": competitor.run_id, "destination": input["destination"],
                    "window_expired": true, "for_seconds": 0, "max_active_leaf_runs": 4}),
                    ToolContext::default(),
                )
                .unwrap();
            assert!(error_of(&full).is_empty(), "{full}");
            assert_eq!(full["admitted"], 0, "{full}");
            assert_eq!(pair.owner_claims().len(), claims_before);
            assert_eq!(owner_jobs.drain_leaf_occupancy().unwrap().occupied, 4);
        }
        workers
            .release(&pull)
            .expect("release the repair pull's worker");
    });

    let superseded = pair.owner_claim(&delivered.claim_id);
    assert_eq!(superseded["claim"]["phase"], "revoked", "{superseded}");
    let repair = pair
        .owner_claims()
        .into_iter()
        .find(|claim| claim["claim"]["repair"]["repairs_claim_id"] == delivered.claim_id.as_str())
        .unwrap_or_else(|| panic!("a repair claim: {:#?}", pair.owner_claims()));
    assert_eq!(repair["claim"]["task_id"], task.as_str());
    assert_eq!(
        repair["claim"]["executed_on"]["machine_id"], OWNER,
        "{repair}"
    );
    assert_eq!(pair.owner_status(&task), "in-progress");
}

crate::scenario_table!(
    the_owner_drain_takes_a_repair_its_original_executor_never_pulls_scenarios,
    the_owner_drain_takes_a_repair_its_original_executor_never_pulls,
    the_owner_repairs_beside_an_implementing_leaf_and_waits_when_full,
    repair_and_backlog_share_the_owner_ceiling_with_other_pulls
);
