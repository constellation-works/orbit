//! A claimed leaf's handoff may owe a criterion the owner's pilot typed
//! post-deploy: the owner accepts it, lands it, and leaves the task in review
//! with the owed criterion listed. A handoff owing a criterion the pilot did
//! not type is refused at acceptance.

use super::*;

use orbit_types::workflow::OwedCriterion;

const HOST: &str = "hm_owner";
const COMMAND: &str = "orbit-latency --samples 20 /api/workflows/auto/readiness";

fn owed() -> OwedCriterion {
    OwedCriterion {
        criterion: 1,
        host: HOST.into(),
        command: COMMAND.into(),
        target: "p95 <= 150 ms".into(),
        candidate_evidence: String::new(),
    }
}

/// Apply the owner pilot's assessment of the pair's task, typing criterion 1
/// post-deploy when `typed`.
fn assess(pair: &Pair, typed: bool) {
    let owner = &pair.wire.owner;
    let task = &pair.tasks[0];
    let action = |name: &str, input: Value| {
        owner
            .run_deterministic(name, &json!({}), &input, ToolContext::default())
            .unwrap()
    };
    let prepared = action(
        "prepare_task_pilot",
        json!({"workspace_path": pair.owner_repo, "task_ids": [task], "base_branch": "main"}),
    );
    let post_deploy = if typed {
        json!([{
            "criterion": 1, "host": HOST, "command": COMMAND,
            "evidence": "Only the deployed dashboard on the owner serves live readiness.",
        }])
    } else {
        json!([])
    };
    let applied = action(
        "apply_task_pilot_results",
        json!({
            "workspace_path": pair.owner_repo, "prepared": prepared,
            "results": [{"partition_index": 0, "task_ids": [task], "tasks": [{
                "task_id": task, "context_files_after": ["file:src/f0.rs"],
                "disposition": "selectors", "recommended_crew": "fixture",
                "recommended_complexity": "low", "confidence": "high",
                "assessment_rationale": "The file contains the repair.",
                "validation_approach": "Land the handoff.",
                "evidence_gaps": [], "reassessment_triggers": [], "blocked_by": [],
                "adr_conflicts": [], "utility_warnings": [], "surface_warnings": [],
                "duplicate_of": null, "already_landed": null,
                "post_deploy": post_deploy,
            }]}],
        }),
    );
    assert_eq!(applied["applied_count"], 1, "{applied}");
}

/// Hand off the pair's claimed leaf owing criterion 1, and return the
/// owner's settlement pass with the handoff and the drain.
fn hand_off_owing(pair: &Pair) -> (Value, TaskHandoff, String) {
    *pair.wire.accept_handoffs.lock().unwrap() = true;
    let drain = pair.run_drain();
    let leaf = pair.running_leaf(&drain, 1);
    let record = pair.admission(&leaf);
    let mut delivered = handoff(&record);
    delivered.owed_criteria = vec![owed()];
    pair.advance(
        &leaf,
        LocalPullMutation::Settle(Box::new(ClaimMutation::AcceptHandoff(delivered.clone()))),
    );
    pair.follower_jobs
        .finalize_job_run(&leaf, JobRunState::Success, Utc::now(), None)
        .unwrap();
    (pair.pass(&drain), delivered, drain)
}

fn authorize_landing(pair: &Pair, delivered: &TaskHandoff) -> (String, ClaimInvocation) {
    let owner = &pair.wire.owner;
    let handoff_id = owner
        .accepted_task_handoff(&delivered.claim_id)
        .unwrap()
        .handoff_id;
    let context = ClaimInvocation::trusted_operator(
        delivered.task_id.clone(),
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
            .mutate_execution_claim(Some(&context), id, &mutation)
            .unwrap();
    }
    (handoff_id, context)
}

fn a_claimed_handoff_owing_a_typed_criterion_lands_in_review() {
    let pair = Pair::new(1);
    assess(&pair, true);
    let (settled, delivered, _) = hand_off_owing(&pair);
    assert!(settled["error"].is_null(), "{settled}");
    let owner = &pair.wire.owner;
    let (handoff_id, _) = authorize_landing(&pair, &delivered);
    RuntimeHost::record_handoff_landing(owner, &orbit_engine::HandoffLandingUpdate {
        handoff_id,
        step: orbit_engine::HandoffLandingStep::Complete,
        observed: Some(delivered.candidate.clone()),
        evidence: json!({"delivery": {"merge_commit": "e".repeat(40), "pr_number": "42"}, "pull_request": 42}).to_string(),
    })
    .unwrap();

    let task = pair.owner_task(&delivered.task_id);
    assert_eq!(task["status"], "review", "the landed task owes criterion 1");
    assert_eq!(task["owed_criteria"][0]["state"], "owed", "{task}");
    assert_eq!(task["owed_criteria"][0]["host"], HOST, "{task}");
    assert_eq!(task["owed_criteria"][0]["command"], COMMAND, "{task}");
    assert_eq!(
        task["owed_criteria"][0]["run_id"],
        delivered.run_id.as_str(),
        "{task}"
    );
}

fn a_claimed_handoff_owing_an_untyped_criterion_is_refused() {
    let pair = Pair::new(1);
    assess(&pair, false);
    let (settled, delivered, drain) = hand_off_owing(&pair);
    assert_eq!(settled["settlement_refused"], 1, "{settled}");
    let refused = pair
        .follower
        .pull_drain_refused_settlements(&drain)
        .unwrap();
    assert!(
        refused[0]
            .reason
            .contains("not one its task-pilot assessment typed post-deploy"),
        "{refused:?}"
    );
    let task = pair.owner_task(&delivered.task_id);
    assert_ne!(task["status"], "review", "{task}");
    assert!(task.get("owed_criteria").is_none(), "{task}");
}

/// A repair or replacement must honor the accepted history. Operator results
/// are recorded after the original landing releases its claim; the automatic
/// repair case keeps the original, still-unmeasured obligation.
fn a_repair_omitting_criteria_preserves_the_prior_operator_obligation() {
    for outcome in ["owed", "failed", "met"] {
        let pair = Pair::new(1);
        assess(&pair, true);
        let (settled, first, drain) = hand_off_owing(&pair);
        assert!(settled["error"].is_null(), "{settled}");
        let owner = &pair.wire.owner;
        let (first_id, _) = authorize_landing(&pair, &first);
        if outcome == "owed" {
            RuntimeHost::record_handoff_landing(
                owner,
                &orbit_engine::HandoffLandingUpdate {
                    handoff_id: first_id,
                    step: orbit_engine::HandoffLandingStep::Stop { repairable: true },
                    observed: None,
                    evidence: "candidate conflicts with its moved base".into(),
                },
            )
            .unwrap();
            assert_eq!(pair.owner_status(&first.task_id), "in-progress");
        } else {
            RuntimeHost::record_handoff_landing(owner, &orbit_engine::HandoffLandingUpdate {
                handoff_id: first_id,
                step: orbit_engine::HandoffLandingStep::Complete,
                observed: Some(first.candidate.clone()),
                evidence: json!({"delivery": {"merge_commit": "e".repeat(40), "pr_number": "42"}, "pull_request": 42}).to_string(),
            }).unwrap();
            assert_eq!(pair.owner_status(&first.task_id), "review");
            record_measurement(&pair, &first.task_id, outcome);
            owner
                .update_task_as_human(
                    &first.task_id,
                    orbit_core::application::task::TaskUpdateParams {
                        status: Some(orbit_types::task::TaskStatus::Backlog),
                        ..Default::default()
                    },
                    "human:fixture".into(),
                )
                .unwrap();
        }

        let leaf = pair.running_leaf(&drain, 1);
        let record = pair.admission(&leaf);
        assert_eq!(
            record
                .receipt
                .as_ref()
                .unwrap()
                .claim
                .as_ref()
                .unwrap()
                .repair
                .is_some(),
            outcome == "owed",
        );
        let replacement = handoff(&record);
        // Exercise the persisted handoff wire contract's omitted-field default.
        let mut wire = serde_json::to_value(&replacement).unwrap();
        wire.as_object_mut().unwrap().remove("owed_criteria");
        let replacement: TaskHandoff = serde_json::from_value(wire).unwrap();
        assert!(replacement.owed_criteria.is_empty());
        pair.advance(
            &leaf,
            LocalPullMutation::Settle(Box::new(ClaimMutation::AcceptHandoff(replacement.clone()))),
        );
        pair.follower_jobs
            .finalize_job_run(&leaf, JobRunState::Success, Utc::now(), None)
            .unwrap();
        let settled = pair.pass(&drain);
        assert!(settled["error"].is_null(), "{settled}");
        let (handoff_id, _) = authorize_landing(&pair, &replacement);
        RuntimeHost::record_handoff_landing(owner, &orbit_engine::HandoffLandingUpdate {
            handoff_id,
            step: orbit_engine::HandoffLandingStep::Complete,
            observed: Some(replacement.candidate.clone()),
            evidence: json!({"delivery": {"merge_commit": "e".repeat(40), "pr_number": "42"}, "pull_request": 42}).to_string(),
        }).unwrap();
        let shown = pair.owner_task(&first.task_id);
        assert_eq!(
            shown["status"],
            if outcome == "met" { "done" } else { "review" },
            "a replacement must honor the persisted {outcome} obligation: {shown}"
        );
        assert_eq!(shown["owed_criteria"][0]["state"], outcome, "{shown}");
        assert_eq!(shown["owed_criteria"][0]["run_id"], first.run_id, "{shown}");
        assert_eq!(shown["owed_criteria"][0]["criterion"], 1, "{shown}");
        assert_eq!(shown["owed_criteria"][0]["host"], HOST, "{shown}");
        assert_eq!(shown["owed_criteria"][0]["command"], COMMAND, "{shown}");
        if outcome != "met" {
            record_measurement(&pair, &first.task_id, "met");
            let completed = orbit_engine::execute_deterministic_action(
                owner,
                "task_complete",
                &json!({}),
                &json!({"run_id": replacement.run_id, "task_id": first.task_id}),
                false,
                &Default::default(),
                None,
            )
            .unwrap();
            assert_eq!(
                completed["completed_task_ids"],
                json!([first.task_id]),
                "{completed}"
            );
            let shown = pair.owner_task(&first.task_id);
            assert_eq!(shown["status"], "done", "{shown}");
            assert_eq!(shown["owed_criteria"][0]["state"], "met", "{shown}");
        }
    }
}

fn record_measurement(pair: &Pair, task: &str, outcome: &str) {
    pair.wire
        .owner
        .update_task_as_human(
            task,
            orbit_core::application::task::TaskUpdateParams {
                comment: Some(format!(
                    "post-deploy: {outcome} 1\nMeasured on the deployed owner host."
                )),
                ..Default::default()
            },
            "human:fixture".into(),
        )
        .unwrap();
}

// Related boundary scenarios share the isolated table fixture.
crate::scenario_table!(
    a_claimed_handoff_owing_a_typed_criterion_lands_in_review_scenarios,
    a_claimed_handoff_owing_a_typed_criterion_lands_in_review,
    a_claimed_handoff_owing_an_untyped_criterion_is_refused,
    a_repair_omitting_criteria_preserves_the_prior_operator_obligation
);
