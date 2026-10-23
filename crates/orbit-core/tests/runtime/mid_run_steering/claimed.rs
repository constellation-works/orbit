//! Mid-run steering for a claimed leaf (design §3, spec invariant 5): the
//! leaf's prompts and its promotion gate read the task, its comment log and
//! the leaf's watermark from the owner, and record each snapshot and
//! delivery there. The fixture's own store is only the follower's replica: a
//! comment written there alone is never steering, and an owner the gate
//! cannot read fails the run before `commit` with nothing delivered.
//!
//! The owner is simulated: it answers the leaf's host-owned projections from
//! its own task record, comment log and claim steering record, kept apart
//! from the replica. The distributed-drain suite drives the same channel
//! against a real owner's claim journal.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_store::contracts::{ClaimSteeringReading, ClaimSteeringRecord};
use orbit_tools::OwnerCoordinator;
use orbit_types::task::{Task, TaskComment};
use orbit_types::tool::ToolSessionContext;
use serde_json::{Value, json};

use super::gate::{Pipeline, admitted, history_events, pipeline_head, rounds};
use super::{ENABLED, Fixture};

const CLAIMED_PIPELINES: [&str; 2] = ["task_claimed_local_pipeline", "task_claimed_pr_pipeline"];

/// The owner a claimed leaf's binding reaches, holding the authoritative
/// task record, its comment log and the claim's steering record.
pub(super) struct SimulatedOwner {
    task: Task,
    comments: Mutex<Vec<TaskComment>>,
    steering: Mutex<ClaimSteeringReading>,
    records: Mutex<Vec<ClaimSteeringRecord>>,
    unreachable: AtomicBool,
}

impl SimulatedOwner {
    /// The owner of `fx`'s task, as admission left it, with no comments.
    fn of(fx: &Fixture) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            task: fx.runtime.get_task(&fx.task).unwrap(),
            comments: Mutex::default(),
            steering: Mutex::default(),
            records: Mutex::default(),
            unreachable: AtomicBool::new(false),
        })
    }

    /// An operator's comment on the owner's record.
    fn operator_comment(&self, message: &str) {
        self.comments.lock().unwrap().push(TaskComment {
            at: Utc::now(),
            by: "human:operator".into(),
            message: message.into(),
            operation_id: None,
        });
    }

    fn deliveries(&self) -> Vec<ClaimSteeringRecord> {
        self.records
            .lock()
            .unwrap()
            .iter()
            .filter(|record| matches!(record, ClaimSteeringRecord::Delivery { .. }))
            .cloned()
            .collect()
    }

    fn steering_calls(&self) -> usize {
        self.records.lock().unwrap().len()
    }
}

impl OwnerCoordinator for SimulatedOwner {
    fn call(
        &self,
        name: &str,
        input: Value,
        session: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        if self.unreachable.load(Ordering::SeqCst) {
            return Err(OrbitError::Execution(
                "the simulated owner is unreachable".into(),
            ));
        }
        assert!(
            session.worker_host_call,
            "steering crosses only the host's own channel: {name} {input}"
        );
        assert_eq!(input["id"], self.task.id, "{name} {input}");
        let value = match (name, input["_worker_read"].as_str()) {
            ("orbit.task.show", Some("task")) => serde_json::to_value(&self.task),
            ("orbit.task.show", Some("comments")) => {
                serde_json::to_value(&*self.comments.lock().unwrap())
            }
            ("orbit.task.show", Some("history")) => Ok(json!([])),
            ("orbit.task.show", Some("steering")) => {
                serde_json::to_value(&*self.steering.lock().unwrap())
            }
            ("orbit.task.update", None) => {
                let record: ClaimSteeringRecord =
                    serde_json::from_value(input["_worker_steering"].clone()).unwrap();
                let mut steering = self.steering.lock().unwrap();
                let watermark = steering.claim.get_or_insert_with(Default::default);
                match &record {
                    ClaimSteeringRecord::Snapshot {
                        step_id,
                        newest,
                        built_at,
                    } => watermark.record(step_id, *newest, *built_at),
                    ClaimSteeringRecord::Delivery {
                        step_id,
                        through,
                        delivered,
                        built_at,
                        ..
                    } => watermark.record_delivery(step_id, *through, delivered, *built_at),
                }
                self.records.lock().unwrap().push(record);
                Ok(json!({}))
            }
            _ => panic!("unexpected owner call {name} {input}"),
        };
        Ok(value.unwrap())
    }
}

fn steering_text(prompt: &Value) -> Option<&str> {
    prompt["steering"]["comments"].as_str()
}

fn a_claimed_leaf_steers_from_the_owner_record_and_never_certifies_its_replica() {
    for pipeline in CLAIMED_PIPELINES {
        // Off, the leaf never asks its owner about steering.
        let fx = admitted("");
        let run = fx.run(None);
        let owner = SimulatedOwner::of(&fx);
        let mut host = Pipeline::new(&fx, &run);
        host.owner = Some(owner.clone());
        let late = owner.clone();
        host.on_implement = Box::new(move |_, _| late.operator_comment("STEER: steering is off."));
        let outcome = host.execute(&pipeline_head(pipeline), None);
        assert!(outcome.success, "{pipeline}: {outcome:?}");
        assert!(host.labels().iter().any(|label| label == "commit"));
        assert_eq!(owner.steering_calls(), 0, "{pipeline}: off is inert");

        // A comment only the replica holds is not steering: the gate reads
        // the owner's log, which has none, and commits without a round.
        let fx = admitted(ENABLED);
        let run = fx.run(None);
        let owner = SimulatedOwner::of(&fx);
        let mut host = Pipeline::new(&fx, &run);
        host.owner = Some(owner.clone());
        host.on_implement = Box::new(|fx, _| {
            fx.operator_comment("REPLICA: written only to the follower's copy.");
        });
        let outcome = host.execute(&pipeline_head(pipeline), None);
        assert!(outcome.success, "{pipeline}: {outcome:?}");
        assert_eq!(
            rounds(&fx, &run),
            0,
            "{pipeline}: the replica spent no round"
        );
        assert!(owner.deliveries().is_empty(), "{pipeline}");
        assert!(
            owner.steering.lock().unwrap().claim.is_some(),
            "{pipeline}: the owner holds the leaf's snapshot"
        );
        assert!(
            fx.state(&run).steering_watermarks.is_empty(),
            "{pipeline}: the leaf's replica run state holds no watermark"
        );

        // An owner comment written mid-run is outstanding at the gate; its
        // round's prompt carries the owner's comment, never the replica's,
        // and the owner records the delivery.
        let fx = admitted(ENABLED);
        let run = fx.run(None);
        let owner = SimulatedOwner::of(&fx);
        let mut host = Pipeline::new(&fx, &run);
        host.owner = Some(owner.clone());
        let steer = owner.clone();
        host.on_implement = Box::new(move |fx, built| {
            if built == 0 {
                fx.operator_comment("REPLICA: written only to the follower's copy.");
                steer.operator_comment("STEER: rename the flag before this lands.");
            }
        });
        let outcome = host.execute(&pipeline_head(pipeline), None);
        assert!(outcome.success, "{pipeline}: {outcome:?}");
        assert_eq!(rounds(&fx, &run), 1, "{pipeline}");
        let prompts = host.prompts.lock().unwrap();
        assert_eq!(prompts.len(), 2, "{pipeline}");
        assert_eq!(steering_text(&prompts[0]), None, "{pipeline}");
        let block = steering_text(&prompts[1]).expect("the round's prompt is steered");
        assert!(
            block.contains("STEER: rename the flag"),
            "{pipeline}: {block}"
        );
        assert!(!block.contains("REPLICA"), "{pipeline}: {block}");
        let deliveries = owner.deliveries();
        assert_eq!(deliveries.len(), 1, "{pipeline}");
        let ClaimSteeringRecord::Delivery { delivered, .. } = &deliveries[0] else {
            unreachable!()
        };
        assert_eq!(delivered[0].position, 0, "{pipeline}: the owner's comment");
        assert!(
            history_events(&fx, "steering_delivered").is_empty(),
            "{pipeline}: the replica records no delivery"
        );
    }
}

fn an_owner_unreadable_at_the_gate_fails_the_claimed_leaf_closed() {
    for pipeline in CLAIMED_PIPELINES {
        let fx = admitted(ENABLED);
        let run = fx.run(None);
        let owner = SimulatedOwner::of(&fx);
        let mut host = Pipeline::new(&fx, &run);
        host.owner = Some(owner.clone());
        let lost = owner.clone();
        host.on_implement = Box::new(move |_, _| {
            lost.operator_comment("STEER: written as the owner link drops.");
            lost.unreachable.store(true, Ordering::SeqCst);
        });
        let outcome = host.execute(&pipeline_head(pipeline), None);

        assert!(
            !outcome.success,
            "{pipeline}: an unread owner cannot commit"
        );
        assert!(
            !host.labels().iter().any(|label| label == "commit"),
            "{pipeline}"
        );
        assert_eq!(outcome.pipeline["commit"]["decision"], "unreadable");
        let message = outcome.message.unwrap();
        assert!(
            message.starts_with("[steering_gate_unreadable]")
                && message.contains("from its owner, which could not answer")
                && message.contains("the simulated owner is unreachable"),
            "{pipeline}: the outcome names the owner read: {message}"
        );
        assert_eq!(rounds(&fx, &run), 0, "{pipeline}");
        assert!(owner.deliveries().is_empty(), "{pipeline}");
        assert!(history_events(&fx, "steering_delivered").is_empty());
    }
}

crate::scenario_table!(
    mid_run_steering_claimed_scenarios,
    a_claimed_leaf_steers_from_the_owner_record_and_never_certifies_its_replica,
    an_owner_unreadable_at_the_gate_fails_the_claimed_leaf_closed
);
