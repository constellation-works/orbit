//! Mid-run steering for a claimed leaf against a real owner's claim journal
//! (`docs/design/mid-run-steering/2_design.md` §3).
//!
//! The leaf's prompts and promotion gate run on the follower under its
//! claim's worker binding. They read the owner's comment log and the
//! watermark the owner holds for the claim, and every snapshot and delivery
//! the leaf records lands on the owner through that claim: the delivery as a
//! `steering_delivered` task history event. Settlement keeps the watermark,
//! including a settlement the owner makes for a leaf it never ran, so the
//! task's next claim, or the owner's own later run, starts from it and never
//! delivers the same comment again.

use super::*;

use orbit_core::application::task::TaskUpdateParams;
use orbit_engine::SteeringGateReading;
use orbit_types::tool::WorkerInvocation;
use orbit_types::workflow::CommentLogMark;

use super::claimed_review::ToOwner;

const STEERING: &str = "\n[workflow.mid_run_steering]\nenabled = true\n";

/// An operator's comment on the owner's task, which the owner takes even
/// while an execution claim protects the task [ORB-16723].
fn operator_comment(owner: &OrbitRuntime, task: &str, message: &str) -> CommentLogMark {
    owner
        .update_task_as_human(
            task,
            TaskUpdateParams {
                comment: Some(message.into()),
                ..Default::default()
            },
            "human:operator".into(),
        )
        .expect("an operator comments on the owner's task");
    let comments = owner.get_task_comments(task).unwrap();
    CommentLogMark {
        position: (comments.len() - 1) as u64,
        at: comments.last().unwrap().at,
    }
}

fn outstanding(reading: SteeringGateReading) -> Vec<CommentLogMark> {
    match reading {
        SteeringGateReading::Outstanding(marks) => marks,
        SteeringGateReading::Clear => Vec::new(),
        other => panic!("unexpected gate reading {other:?}"),
    }
}

fn owner_deliveries(owner: &OrbitRuntime, task: &str) -> Vec<Value> {
    owner
        .get_task_history(task)
        .unwrap()
        .into_iter()
        .filter(|entry| entry.event == "steering_delivered")
        .map(|entry| serde_json::from_str(entry.note.as_deref().unwrap()).unwrap())
        .collect()
}

/// A launched leaf of `pair`'s drain, the follower runtime bound to its
/// claim as its worker runs, and the worker's claim authority.
struct BoundLeaf {
    drain: String,
    run: String,
    claim_id: String,
    bound: OrbitRuntime,
    worker: ClaimInvocation,
}

impl BoundLeaf {
    fn launch(pair: &Pair) -> Self {
        let drain = pair.run_drain();
        let run = pair.running_leaf(&drain, 1);
        let record = pair.admission(&run);
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
                    bound_run_id: run.clone(),
                },
                Arc::new(ToOwner(pair.wire.owner.clone())),
            )
            .unwrap();
        let job = pair.follower_jobs.get_job_run(&run).unwrap().unwrap();
        pair.follower_jobs
            .write_run_state(
                &run,
                &PipelineState::new(run.clone(), job.job_id, json!({})),
            )
            .unwrap();
        let worker = ClaimInvocation::trusted_worker(
            claim.task_id.clone(),
            claim.claim_id.clone(),
            FOLLOWER.into(),
            Some(ClaimRun {
                machine_id: FOLLOWER.into(),
                run_id: run.clone(),
            }),
        );
        Self {
            drain,
            run,
            claim_id: claim.claim_id,
            bound,
            worker,
        }
    }

    fn prompt(&self, task: &str) -> Value {
        RuntimeHost::task_context_for_agent_input(
            &self.bound,
            &self.run,
            "implement_one",
            &json!({ "task_id": task }),
        )
        .unwrap()
        .expect("a task context")
    }

    fn gate(&self, task: &str) -> Result<SteeringGateReading, OrbitError> {
        RuntimeHost::steering_gate(&self.bound, &self.run, task, None)
    }
}

fn a_claimed_leaf_records_steering_on_its_owner_and_settlement_keeps_it() {
    let pair = Pair::with_configs(STEERING, STEERING, &[None]);
    let owner = pair.wire.owner.clone();

    // The first claim's first prompt is its snapshot, which the owner holds
    // against the claim; the follower's run state holds none.
    let first = BoundLeaf::launch(&pair);
    let task = pair.claimed_task(&first.run);
    assert!(first.prompt(&task).get("steering").is_none());
    let recorded = &pair.owner_claim(&first.claim_id)["steering"];
    assert!(recorded["steps"]["implement_one"].is_object(), "{recorded}");
    assert!(
        pair.follower_jobs
            .read_run_state(&first.run)
            .unwrap()
            .unwrap()
            .steering_watermarks
            .is_empty(),
        "the follower's run state holds no watermark"
    );
    assert!(outstanding(first.gate(&task).unwrap()).is_empty());

    // The leaf fails on this host and its settlement releases the claim;
    // the claim keeps its watermark.
    pair.leaf_fails_with(
        &first.run,
        "[transient_failure] action pin check could not connect (status 000)",
    );
    let pass = pair.pass_over(&first.drain, json!({"window_expired": true}));
    assert_eq!(pair.owner_status(&task), "backlog", "{pass}");
    let kept = &pair.owner_claim(&first.claim_id)["steering"];
    assert_eq!(kept, recorded, "the release keeps the claim's watermark");

    // The operator steers the unclaimed task. The next claim's leaf built
    // no prompt yet, and the comment predates its run, yet the gate finds it
    // outstanding against the first claim's watermark; the round's prompt
    // delivers it and the owner records the delivery.
    let steer = operator_comment(&owner, &task, "STEER: rename the flag before this lands.");
    let second = BoundLeaf::launch(&pair);
    assert_eq!(
        pair.claimed_task(&second.run),
        task,
        "the task is pulled again"
    );
    let found = outstanding(second.gate(&task).unwrap());
    assert_eq!(found, vec![steer], "the inherited baseline");
    RuntimeHost::admit_steering_round(&second.bound, &second.run, &task, &found, None).unwrap();
    let steered = second.prompt(&task);
    let block = steered["steering"]["comments"].as_str().unwrap();
    assert!(block.contains("STEER: rename the flag"), "{block}");
    let deliveries = owner_deliveries(&owner, &task);
    assert_eq!(deliveries.len(), 1, "{deliveries:?}");
    assert_eq!(deliveries[0]["run_id"], second.run);
    assert_eq!(deliveries[0]["claim_id"], second.claim_id);
    assert_eq!(deliveries[0]["machine_id"], FOLLOWER);
    assert_eq!(deliveries[0]["delivered"][0]["position"], steer.position);
    assert!(outstanding(second.gate(&task).unwrap()).is_empty());

    // The owner settles the second claim itself, as it does for a leaf
    // whose host went away. The watermark survives, and the leaf, whose
    // claim the owner no longer runs, certifies nothing.
    owner
        .mutate_execution_claim(
            Some(&second.worker),
            "release",
            &ClaimMutation::Release(ClaimEvidence {
                summary: Some("The leaf's host stopped answering.".into()),
                ..ClaimEvidence::default()
            }),
        )
        .expect("the owner releases the claim");
    let kept = &pair.owner_claim(&second.claim_id)["steering"];
    assert_eq!(kept["comment"]["position"], steer.position, "{kept}");
    assert_eq!(owner_deliveries(&owner, &task).len(), 1);
    let refused = second.gate(&task).unwrap_err().to_string();
    assert!(
        refused.contains("from its owner") && refused.contains("stale_claim"),
        "{refused}"
    );

    // The owner's own later run built no prompt. Its baseline is the
    // latest claim's watermark: the delivered comment stays delivered, and
    // one no leaf carried is outstanding.
    let gap = operator_comment(&owner, &task, "GAP: written after the leaf's last prompt.");
    let jobs = orbit_store::compose::workspace_job_run_store(
        owner.sqlite_store().unwrap(),
        owner.workspace_id().unwrap(),
    );
    let later = jobs
        .insert_job_run(
            "task_pr_pipeline",
            1,
            Utc::now(),
            Some(json!({ "task_id": task })),
            None,
        )
        .unwrap();
    jobs.mark_job_run_running(&later.run_id, Utc::now(), std::process::id())
        .unwrap();
    jobs.write_run_state(
        &later.run_id,
        &PipelineState::new(later.run_id.clone(), later.job_id, json!({})),
    )
    .unwrap();
    let reading = RuntimeHost::steering_gate(&owner, &later.run_id, &task, None).unwrap();
    assert_eq!(outstanding(reading), vec![gap]);
}

/// [ORB-16723] The operator steers while the leaf holds its claim: the owner
/// admits the comment, the leaf's gate finds it outstanding against the
/// watermark the owner holds for the live claim, and the next prompt
/// delivers it.
fn an_operator_comment_under_a_live_claim_is_the_leafs_outstanding_steering() {
    let pair = Pair::with_configs(STEERING, STEERING, &[None]);
    let owner = pair.wire.owner.clone();
    let leaf = BoundLeaf::launch(&pair);
    let task = pair.claimed_task(&leaf.run);
    assert!(leaf.prompt(&task).get("steering").is_none());
    assert!(outstanding(leaf.gate(&task).unwrap()).is_empty());

    let steer = operator_comment(&owner, &task, "STEER: keep the flag name stable.");
    let found = outstanding(leaf.gate(&task).unwrap());
    assert_eq!(
        found,
        vec![steer],
        "the claimed leaf's outstanding steering"
    );
    RuntimeHost::admit_steering_round(&leaf.bound, &leaf.run, &task, &found, None).unwrap();
    let block = leaf.prompt(&task)["steering"]["comments"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        block.contains("STEER: keep the flag name stable."),
        "{block}"
    );
    let deliveries = owner_deliveries(&owner, &task);
    assert_eq!(deliveries.len(), 1, "{deliveries:?}");
    assert_eq!(deliveries[0]["claim_id"], leaf.claim_id);
    assert!(outstanding(leaf.gate(&task).unwrap()).is_empty());
}

crate::scenario_table!(
    a_claimed_leaf_records_steering_on_its_owner_scenarios,
    a_claimed_leaf_records_steering_on_its_owner_and_settlement_keeps_it,
    an_operator_comment_under_a_live_claim_is_the_leafs_outstanding_steering
);
