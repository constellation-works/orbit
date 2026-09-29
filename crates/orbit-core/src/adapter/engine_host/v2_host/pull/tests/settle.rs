//! [ORB-13663] Settlement does not depend on the drain that admitted a claim.
//!
//! These drive the production follower path end to end against a scripted
//! owner transport: admissions are made through [`RoutedPullPeer`], a leaf
//! terminalizes through its bound worker runtime's own `RuntimeHost`
//! finalization, and cancel / stop go through the public runtime entry points
//! the CLI and dashboard call.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_engine::RuntimeHost;
use orbit_store::contracts::*;
use orbit_tools::{DrainOwnerTransport, OwnerCoordinator};
use orbit_types::tool::ToolSessionContext;
use orbit_types::workflow::automation::SourceRevision;
use orbit_types::workflow::handoff::{
    HandoffCandidate, HandoffDelivery, HandoffReview, HandoffReviewDisposition, TaskHandoff,
};
use orbit_types::workflow::{JobRunState, PipelineState, ReviewTiming};
use serde_json::{Value, json};

use super::super::adapters::{LeafPullLauncher, RoutedPullPeer};
use super::super::drain::{PullDrain, PullLauncher};
use super::super::refill::pull_refill;
use super::drain::isolated_pull_test;
use crate::OrbitRuntime;
use crate::adapter::engine_host::v2_host::test_support::runtime_with_workspace_layout;
use crate::application::distributed::{PULL_DRAIN_JOB, PullSettlementEntry};
use crate::application::job::DrainAdmissionsStopRequest;

const OWNER: &str = "hm_owner";
const FOLLOWER: &str = "hm_follower";

/// The owner as a follower's federated route reaches it. It admits one task
/// per pull, records every bind and settlement it is sent, and answers
/// receipt lookups from what it admitted.
#[derive(Default)]
struct Owner {
    /// Every call attempted, delivered or not.
    calls: Mutex<Vec<(String, Value)>>,
    /// Settlements the owner actually received.
    settled: Mutex<Vec<Value>>,
    receipts: Mutex<BTreeMap<String, AdmissionReceipt>>,
    /// Fail the next bind as a lost delivery.
    lose_next_bind: Mutex<bool>,
    /// Answer every call as an unreachable destination.
    unreachable: Mutex<bool>,
    /// Answer new pulls with nothing ready.
    idle: Mutex<bool>,
    /// Answer every pull as an unreachable destination, while the probe and
    /// the other tools still answer.
    pull_unreachable: Mutex<bool>,
    /// Runs once, at the next bind call, before it is answered: something that
    /// happens on this host while the pass waits on the owner.
    on_bind: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl Owner {
    fn tool_calls(&self, tool: &str) -> Vec<Value> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(name, _)| name == tool)
            .map(|(_, input)| input.clone())
            .collect()
    }

    /// Each settlement the owner received, by claim, as `Fail` or
    /// `AcceptHandoff`. Deliveries that did not arrive are not counted.
    fn settlements(&self) -> BTreeMap<String, Vec<&'static str>> {
        let mut settled = BTreeMap::<String, Vec<&'static str>>::new();
        for input in self.settled.lock().unwrap().iter() {
            let claim = input["claim_id"].as_str().expect("claim").to_string();
            let kind = if input["settlement"].get("Fail").is_some() {
                "Fail"
            } else if input["settlement"].get("AcceptHandoff").is_some() {
                "AcceptHandoff"
            } else {
                panic!("unexpected settlement {input}")
            };
            settled.entry(claim).or_default().push(kind);
        }
        settled
    }

    fn receipt(&self, request: &AdmissionRequest) -> AdmissionReceipt {
        let mut receipts = self.receipts.lock().unwrap();
        let task = format!("task-{}", receipts.len() + 1);
        let idle = *self.idle.lock().unwrap();
        receipts
            .entry(request.request_id.clone())
            .or_insert_with(|| AdmissionReceipt {
                schema_version: 1,
                request: request.clone(),
                machine_id: FOLLOWER.into(),
                claim: (!idle).then(|| ExecutionClaim {
                    claim_id: format!("claim-{task}"),
                    task_id: task.clone(),
                    request_id: request.request_id.clone(),
                    executed_on: ExecutionLocation {
                        machine_id: FOLLOWER.into(),
                        machine_name: None,
                    },
                    run_context: request.run_context.clone(),
                    footprint: vec!["file:src.rs".into()],
                    reservation_id: format!("reservation-{task}"),
                    reservation_expires_at: "later".into(),
                    phase: ExecutionClaimPhase::Claimed,
                }),
                task: (!idle).then(|| AdmissionTaskSummary {
                    id: task.clone(),
                    title: task.clone(),
                    complexity: None,
                    crew: None,
                    context_files: vec!["file:src.rs".into()],
                }),
                invalid_candidates: vec![],
                deferred_conflicts: vec![],
                queue_depth: 0,
            })
            .clone()
    }
}

impl DrainOwnerTransport for Owner {
    fn call(&self, selector: &str, name: &str, input: Value) -> Result<Value, OrbitError> {
        assert_eq!(selector, format!("{OWNER}/ws"));
        self.calls
            .lock()
            .unwrap()
            .push((name.to_string(), input.clone()));
        if *self.unreachable.lock().unwrap() {
            return Err(OrbitError::UnreachableDestination(format!(
                "{selector}: ssh: connect to host timed out"
            )));
        }
        match name {
            "orbit.drain.probe" => Ok(json!({
                "owner_machine_id": OWNER,
                "admits": true,
                "ship": AdmissionShipContract {
                    mode: "pr".into(),
                    base_branch: "main".into(),
                    landing_branch: "main".into(),
                    review_policy: "none".into(),
                    completion: "review".into(),
                    authorization_reference: None,
                },
            })),
            "orbit.task.pull" if *self.pull_unreachable.lock().unwrap() => {
                Err(OrbitError::UnreachableDestination(format!(
                    "{selector}: ssh: connect to host timed out"
                )))
            }
            "orbit.task.pull" => {
                let request: AdmissionRequest = serde_json::from_value(input).expect("request");
                Ok(json!({ "receipt": self.receipt(&request) }))
            }
            "orbit.drain.claim.bind" => {
                if let Some(hook) = self.on_bind.lock().unwrap().take() {
                    hook();
                }
                if std::mem::take(&mut *self.lose_next_bind.lock().unwrap()) {
                    return Err(OrbitError::Execution("lost bind response".into()));
                }
                Ok(json!({}))
            }
            "orbit.drain.claim.settle" => {
                self.settled.lock().unwrap().push(input);
                Ok(json!({}))
            }
            "orbit.drain.receipt.lookup" => {
                let id = input["request_id"].as_str().expect("request id");
                Ok(match self.receipts.lock().unwrap().get(id) {
                    Some(receipt) => {
                        json!({ "outcome": "found", "receipt": receipt, "current_claim": null })
                    }
                    None => json!({ "outcome": "not_found" }),
                })
            }
            other => panic!("unexpected owner tool {other}"),
        }
    }

    fn worker_coordinator(&self) -> Arc<dyn OwnerCoordinator> {
        Arc::new(NoWorkerRoute)
    }
}

struct NoWorkerRoute;

impl OwnerCoordinator for NoWorkerRoute {
    fn call(&self, name: &str, _: Value, _: ToolSessionContext) -> Result<Value, OrbitError> {
        panic!("these fixtures route no worker tool, got {name}")
    }
}

/// Acknowledges a launch without starting anything, so the leaf stays live
/// (pending) until the fixture ends it through its bound worker.
struct AcknowledgingLauncher;

impl PullLauncher for AcknowledgingLauncher {
    fn launch(&self, _admission: &LocalPullAdmission) -> Result<(), OrbitError> {
        Ok(())
    }
    fn cancel_queued(&self, _admission: &LocalPullAdmission) -> Result<(), OrbitError> {
        panic!("a live drain never cancels its queued leaves")
    }
}

fn follower(owner: &Arc<Owner>) -> (tempfile::TempDir, OrbitRuntime) {
    let (temp, runtime, _repo) = runtime_with_workspace_layout();
    (temp, runtime.with_drain_owner_transport(owner.clone()))
}

fn destination() -> PullDestination {
    PullDestination {
        owner_machine_id: OWNER.into(),
        owner_workspace_id: "ws".into(),
        selector: format!("{OWNER}/ws"),
        execution_machine_id: FOLLOWER.into(),
    }
}

/// A follower pull drain run and the request template it admits under.
fn pull_drain(runtime: &OrbitRuntime) -> (String, AdmissionRequest) {
    let jobs = runtime.stores().jobs();
    let run = jobs
        .insert_job_run(PULL_DRAIN_JOB, 1, Utc::now(), None, None)
        .expect("drain run");
    jobs.write_run_state(
        &run.run_id,
        &PipelineState::new(run.run_id.clone(), run.job_id, json!({})),
    )
    .expect("drain state");
    let template = AdmissionRequest {
        request_id: String::new(),
        caller_version: "1".into(),
        caller_schema: 1,
        caller_review_policy: "none".into(),
        run_context: AdmissionRunContext {
            run_id: run.run_id.clone(),
            job_name: PULL_DRAIN_JOB.into(),
            machine_name: None,
        },
        ship: AdmissionShipContract {
            mode: "pr".into(),
            base_branch: "main".into(),
            landing_branch: "main".into(),
            review_policy: "none".into(),
            completion: "review".into(),
            authorization_reference: None,
        },
    };
    (run.run_id, template)
}

fn records(runtime: &OrbitRuntime) -> Vec<LocalPullAdmission> {
    runtime
        .stores()
        .jobs()
        .local_pull_admissions()
        .expect("admissions")
}

fn claim_of(record: &LocalPullAdmission) -> String {
    record
        .receipt
        .as_ref()
        .and_then(|receipt| receipt.claim.as_ref())
        .expect("claim")
        .claim_id
        .clone()
}

fn leaf_of(record: &LocalPullAdmission) -> String {
    record.leaf_run_id.clone().expect("leaf")
}

/// The leaf's own worker process: this runtime bound to the admission's
/// claim and run, exactly as `LeafPullLauncher` binds a spawned worker.
fn bound_worker(runtime: &OrbitRuntime, record: &LocalPullAdmission) -> OrbitRuntime {
    LeafPullLauncher { runtime }
        .bound_runtime(record)
        .expect("bound worker runtime")
}

fn handoff(record: &LocalPullAdmission) -> TaskHandoff {
    let claim = record
        .receipt
        .as_ref()
        .and_then(|receipt| receipt.claim.as_ref())
        .expect("claim");
    TaskHandoff {
        schema_version: 1,
        workspace_id: record.destination.owner_workspace_id.clone(),
        task_id: claim.task_id.clone(),
        claim_id: claim.claim_id.clone(),
        machine_id: claim.executed_on.machine_id.clone(),
        run_id: leaf_of(record),
        candidate: HandoffCandidate {
            repository: "owner/repository".into(),
            source_branch: format!("orbit/{}", claim.task_id),
            base_branch: "main".into(),
            landing_branch: "main".into(),
            candidate: SourceRevision {
                commit: "a".repeat(40),
                tree: "b".repeat(40),
            },
            base: SourceRevision {
                commit: "c".repeat(40),
                tree: "d".repeat(40),
            },
            delivery: HandoffDelivery::PullRequest { number: 42 },
        },
        review: HandoffReview {
            policy: ReviewTiming::None,
            disposition: HandoffReviewDisposition::NotRequired,
        },
        execution_summary: "Outcome: success".into(),
        validation: vec![],
    }
}

/// End a leaf the way its worker does: running, then terminal through the
/// runtime's own finalization — which is where settlement now happens.
fn worker_ends_leaf(worker: &OrbitRuntime, leaf: &str, state: JobRunState) {
    worker
        .stores()
        .jobs()
        .mark_job_run_running(leaf, Utc::now(), std::process::id())
        .expect("leaf running");
    RuntimeHost::finalize_job_run(worker, leaf, state, Utc::now(), None).expect("leaf terminal");
}

fn outcomes(entries: &[PullSettlementEntry]) -> Vec<(Option<String>, String)> {
    entries
        .iter()
        .map(|entry| (entry.task_id.clone(), entry.outcome.clone()))
        .collect()
}

/// [ORB-13663] Cancel a follower drain while its leaves are live: each leaf
/// that later succeeds delivers its handoff, each that fails delivers a
/// failure, and the claim the drain had not launched yet is ended — all with
/// no further drain pass.
#[test]
fn leaves_of_a_cancelled_drain_settle_themselves_with_no_further_drain() {
    if isolated_pull_test(
        "adapter::engine_host::v2_host::pull::tests::settle::leaves_of_a_cancelled_drain_settle_themselves_with_no_further_drain",
    ) {
        return;
    }
    let owner = Arc::new(Owner::default());
    let (_temp, runtime) = follower(&owner);
    let jobs = runtime.stores().jobs();
    let (drain_run, template) = pull_drain(&runtime);
    let peer = RoutedPullPeer {
        transport: owner.clone(),
    };
    let drain = PullDrain {
        jobs,
        peer: &peer,
        launcher: &AcknowledgingLauncher,
    };
    assert_eq!(
        drain.refill(&destination(), &template, 2).expect("admit"),
        2
    );
    // A third claim whose bind response is lost: created locally, never
    // launched.
    *owner.lose_next_bind.lock().unwrap() = true;
    assert!(drain.refill(&destination(), &template, 3).is_err());
    let admitted = records(&runtime);
    assert_eq!(
        admitted
            .iter()
            .map(|record| record.phase)
            .collect::<Vec<_>>(),
        [
            LocalPullPhase::Launched,
            LocalPullPhase::Launched,
            LocalPullPhase::Created
        ]
    );
    let (succeeds, fails, queued) = (&admitted[0], &admitted[1], &admitted[2]);

    let cancel = runtime
        .cancel_job_run_with_reason(&drain_run, "dashboard", "web", None)
        .expect("cancel the drain");
    assert_eq!(cancel.outcome, "cancelled");
    assert_eq!(
        outcomes(&cancel.pull_settlements),
        [
            (Some("task-1".into()), "leaf_running".into()),
            (Some("task-2".into()), "leaf_running".into()),
            (Some("task-3".into()), "settled".into()),
        ],
        "live leaves are left running; the unlaunched claim is ended"
    );
    assert_eq!(
        jobs.get_job_run(&leaf_of(queued))
            .expect("read")
            .expect("queued leaf")
            .state,
        JobRunState::Cancelled,
        "the queued leaf can never start"
    );
    assert_eq!(
        owner.settlements(),
        BTreeMap::from([(claim_of(queued), vec!["Fail"])])
    );
    let pulls_after_cancel = owner.tool_calls("orbit.task.pull").len();

    // The leaves finish after their drain is gone. Success records its typed
    // handoff (as `claim_handoff` does) before its run terminalizes.
    let worker = bound_worker(&runtime, succeeds);
    RuntimeHost::record_claim_handoff(&worker, &handoff(succeeds)).expect("handoff recorded");
    worker_ends_leaf(&worker, &leaf_of(succeeds), JobRunState::Success);
    worker_ends_leaf(
        &bound_worker(&runtime, fails),
        &leaf_of(fails),
        JobRunState::Failed,
    );

    assert_eq!(
        owner.settlements(),
        BTreeMap::from([
            (claim_of(succeeds), vec!["AcceptHandoff"]),
            (claim_of(fails), vec!["Fail"]),
            (claim_of(queued), vec!["Fail"]),
        ]),
        "every claim settled exactly once, by its own leaf"
    );
    assert!(
        records(&runtime)
            .iter()
            .all(|record| record.phase == LocalPullPhase::Settled),
        "{:?}",
        records(&runtime)
    );
    assert_eq!(
        owner.tool_calls("orbit.task.pull").len(),
        pulls_after_cancel,
        "no drain asked the owner for more work"
    );
    let failure = records(&runtime)
        .into_iter()
        .find(|record| claim_of(record) == claim_of(fails))
        .and_then(|record| record.settlement)
        .expect("failure settlement");
    let ClaimMutation::Fail(evidence) = failure else {
        panic!("a failed leaf settles as a failure");
    };
    assert!(
        evidence
            .summary
            .as_deref()
            .is_some_and(|summary| summary.starts_with("Outcome: failed")
                && summary.contains(&leaf_of(fails))),
        "{evidence:?}"
    );
}

/// Leave the owner exactly the state a cancelled drain left before
/// [ORB-13663]: a finished leaf's handoff recorded but undelivered, and a
/// failed leaf with no settlement at all. Leaves are ended at the store, with
/// no settlement hook, as the old binary did.
fn stranded_by_a_cancelled_drain(
    runtime: &OrbitRuntime,
    owner: &Arc<Owner>,
) -> Vec<LocalPullAdmission> {
    let jobs = runtime.stores().jobs();
    let (drain_run, template) = pull_drain(runtime);
    let peer = RoutedPullPeer {
        transport: owner.clone(),
    };
    let drain = PullDrain {
        jobs,
        peer: &peer,
        launcher: &AcknowledgingLauncher,
    };
    assert_eq!(
        drain.refill(&destination(), &template, 2).expect("admit"),
        2
    );
    jobs.finalize_job_run(&drain_run, JobRunState::Cancelled, Utc::now(), None)
        .expect("drain cancelled");
    let admitted = records(runtime);
    jobs.mutate_local_pull(
        &admitted[0].destination,
        &admitted[0].request.request_id,
        &LocalPullMutation::Settle(Box::new(ClaimMutation::AcceptHandoff(handoff(
            &admitted[0],
        )))),
    )
    .expect("handoff recorded");
    for (record, state) in [
        (&admitted[0], JobRunState::Success),
        (&admitted[1], JobRunState::Failed),
    ] {
        jobs.mark_job_run_running(&leaf_of(record), Utc::now(), std::process::id())
            .expect("leaf running");
        jobs.finalize_job_run(&leaf_of(record), state, Utc::now(), None)
            .expect("leaf ended");
    }
    let stranded = records(runtime);
    assert_eq!(
        stranded
            .iter()
            .map(|record| record.phase)
            .collect::<Vec<_>>(),
        [LocalPullPhase::Settling, LocalPullPhase::Launched]
    );
    assert!(owner.settlements().is_empty());
    stranded
}

/// [ORB-13663] A new drain for the same owner delivers what an earlier,
/// cancelled drain left behind — not only its own admissions.
#[test]
fn a_new_drain_delivers_settlements_a_cancelled_drain_left_behind() {
    if isolated_pull_test(
        "adapter::engine_host::v2_host::pull::tests::settle::a_new_drain_delivers_settlements_a_cancelled_drain_left_behind",
    ) {
        return;
    }
    let owner = Arc::new(Owner::default());
    let (_temp, runtime) = follower(&owner);
    let stranded = stranded_by_a_cancelled_drain(&runtime, &owner);

    *owner.idle.lock().unwrap() = true;
    let (_next_run, next_template) = pull_drain(&runtime);
    let peer = RoutedPullPeer {
        transport: owner.clone(),
    };
    let drain = PullDrain {
        jobs: runtime.stores().jobs(),
        peer: &peer,
        launcher: &AcknowledgingLauncher,
    };
    assert_eq!(
        drain
            .refill(&destination(), &next_template, 5)
            .expect("next drain pass"),
        0
    );
    assert_eq!(
        owner.settlements(),
        BTreeMap::from([
            (claim_of(&stranded[0]), vec!["AcceptHandoff"]),
            (claim_of(&stranded[1]), vec!["Fail"]),
        ])
    );
    let after = records(&runtime);
    assert_eq!(after[0].phase, LocalPullPhase::Settled);
    assert_eq!(after[1].phase, LocalPullPhase::Settled);
}

/// [ORB-13663] The one-time recovery for settlements stranded by an older
/// binary: `orbit run auto --stop` (or cancelling the already-ended drain)
/// delivers them without starting a drain. Delivery to an unreachable owner
/// stays recorded, costs one attempt per owner, and a later pass finishes it.
#[test]
fn stop_and_cancel_deliver_stranded_settlements_without_a_drain() {
    if isolated_pull_test(
        "adapter::engine_host::v2_host::pull::tests::settle::stop_and_cancel_deliver_stranded_settlements_without_a_drain",
    ) {
        return;
    }
    let owner = Arc::new(Owner::default());
    let (_temp, runtime) = follower(&owner);
    let stranded = stranded_by_a_cancelled_drain(&runtime, &owner);
    let stop = || {
        runtime
            .stop_workspace_auto_admissions(DrainAdmissionsStopRequest {
                actor: "cli",
                source: "run_auto_stop",
                reason: None,
                claim_token: None,
            })
            .expect("stop")
    };

    *owner.unreachable.lock().unwrap() = true;
    let calls_before = owner.calls.lock().unwrap().len();
    let unreachable = stop();
    assert_eq!(unreachable.outcome, "idle");
    assert_eq!(
        outcomes(&unreachable.pull_settlements),
        [
            (Some("task-1".into()), "pending_delivery".into()),
            (Some("task-2".into()), "owner_unreachable".into()),
        ]
    );
    assert_eq!(
        owner.calls.lock().unwrap().len(),
        calls_before + 1,
        "one delivery attempt per unreachable owner, not one per admission"
    );
    assert!(
        records(&runtime)
            .iter()
            .all(|record| record.phase != LocalPullPhase::Settled)
    );

    *owner.unreachable.lock().unwrap() = false;
    let cancel = runtime
        .cancel_job_run_with_reason(
            &stranded[0].request.run_context.run_id,
            "cli",
            "run_cancel",
            None,
        )
        .expect("cancel the ended drain");
    assert_eq!(cancel.outcome, "already_terminal");
    assert_eq!(
        outcomes(&cancel.pull_settlements),
        [
            (Some("task-1".into()), "settled".into()),
            (Some("task-2".into()), "settled".into()),
        ]
    );
    assert_eq!(
        owner.settlements(),
        BTreeMap::from([
            (claim_of(&stranded[0]), vec!["AcceptHandoff"]),
            (claim_of(&stranded[1]), vec!["Fail"]),
        ])
    );
    assert!(
        stop().pull_settlements.is_empty(),
        "a settled admission is never delivered again"
    );
    let unique: BTreeSet<_> = owner.settlements().into_keys().collect();
    assert_eq!(unique.len(), 2);
}

/// [ORB-13663] A settle-only pass never takes work from a live drain: while
/// another drain for the same owner is running, a cancelled drain's
/// unlaunched claim is left for it to carry, and its queued leaf is not
/// cancelled.
#[test]
fn a_live_drain_for_the_owner_keeps_a_cancelled_drains_unlaunched_claim() {
    if isolated_pull_test(
        "adapter::engine_host::v2_host::pull::tests::settle::a_live_drain_for_the_owner_keeps_a_cancelled_drains_unlaunched_claim",
    ) {
        return;
    }
    let owner = Arc::new(Owner::default());
    let (_temp, runtime) = follower(&owner);
    let jobs = runtime.stores().jobs();
    let (drain_run, template) = pull_drain(&runtime);
    let peer = RoutedPullPeer {
        transport: owner.clone(),
    };
    let drain = PullDrain {
        jobs,
        peer: &peer,
        launcher: &AcknowledgingLauncher,
    };
    *owner.lose_next_bind.lock().unwrap() = true;
    assert!(drain.refill(&destination(), &template, 1).is_err());
    let queued = records(&runtime).remove(0);
    assert_eq!(queued.phase, LocalPullPhase::Created);
    // A second drain for the same owner is live.
    jobs.insert_job_run(
        PULL_DRAIN_JOB,
        1,
        Utc::now(),
        Some(json!({ "destination": destination() })),
        None,
    )
    .expect("live drain");

    let cancel = runtime
        .cancel_job_run_with_reason(&drain_run, "cli", "run_cancel", None)
        .expect("cancel the first drain");
    assert_eq!(
        outcomes(&cancel.pull_settlements),
        [(Some("task-1".into()), "awaiting_drain".into())]
    );
    assert_eq!(
        jobs.get_job_run(&leaf_of(&queued))
            .expect("read")
            .expect("queued leaf")
            .state,
        JobRunState::Pending,
        "the live drain can still launch it"
    );
    assert!(owner.settlements().is_empty());
}

/// End a leaf at the store, with no settlement hook, as a worker that died
/// would leave it: the next settle-only pass records and delivers its failure.
fn leaf_ends_unrecorded(jobs: &dyn JobRunStoreBackend, record: &LocalPullAdmission) {
    let leaf = leaf_of(record);
    jobs.mark_job_run_running(&leaf, Utc::now(), std::process::id())
        .expect("leaf running");
    jobs.finalize_job_run(&leaf, JobRunState::Failed, Utc::now(), None)
        .expect("leaf ended");
}

/// One admission's local error is that admission's problem. It used to mark
/// the whole owner unreachable, so every admission after it was skipped even
/// though the owner never failed a call.
#[test]
fn a_local_error_on_one_admission_does_not_stop_the_rest_of_the_pass() {
    if isolated_pull_test(
        "adapter::engine_host::v2_host::pull::tests::settle::a_local_error_on_one_admission_does_not_stop_the_rest_of_the_pass",
    ) {
        return;
    }
    let owner = Arc::new(Owner::default());
    let (_temp, runtime) = follower(&owner);
    let jobs = runtime.stores().jobs();
    let (_drain_run, template) = pull_drain(&runtime);
    let peer = RoutedPullPeer {
        transport: owner.clone(),
    };
    let drain = PullDrain {
        jobs,
        peer: &peer,
        launcher: &AcknowledgingLauncher,
    };
    assert_eq!(
        drain.refill(&destination(), &template, 2).expect("admit"),
        2
    );
    let admitted = records(&runtime);
    for record in &admitted {
        leaf_ends_unrecorded(jobs, record);
    }
    // The first admission's leaf record is gone: a local fault, not the owner's.
    jobs.delete_job_run(&leaf_of(&admitted[0]))
        .expect("leaf record removed");

    let entries = runtime.settle_pending_pulls();
    assert_eq!(
        outcomes(&entries),
        [
            (Some("task-1".into()), "pending".into()),
            (Some("task-2".into()), "settled".into()),
        ]
    );
    let detail = entries[0].detail.as_deref().unwrap_or_default();
    assert!(
        detail.contains("disappeared"),
        "the real error is reported: {detail}"
    );
    assert_eq!(
        owner.settlements(),
        BTreeMap::from([(claim_of(&admitted[1]), vec!["Fail"])]),
        "the owner was reachable and got the healthy admission's settlement"
    );
}

/// A settle-only pass reads the live drains once, but delivery can block for
/// the routed timeout. A drain that starts while it waits on the owner is
/// carrying that admission now: its queued leaf must not be cancelled under it.
#[test]
fn a_drain_that_starts_mid_pass_keeps_its_queued_leaf() {
    if isolated_pull_test(
        "adapter::engine_host::v2_host::pull::tests::settle::a_drain_that_starts_mid_pass_keeps_its_queued_leaf",
    ) {
        return;
    }
    let owner = Arc::new(Owner::default());
    let (_temp, runtime) = follower(&owner);
    let jobs = runtime.stores().jobs();
    let (drain_run, template) = pull_drain(&runtime);
    let peer = RoutedPullPeer {
        transport: owner.clone(),
    };
    let drain = PullDrain {
        jobs,
        peer: &peer,
        launcher: &AcknowledgingLauncher,
    };
    *owner.lose_next_bind.lock().unwrap() = true;
    assert!(drain.refill(&destination(), &template, 1).is_err());
    let queued = records(&runtime).remove(0);
    assert_eq!(queued.phase, LocalPullPhase::Created);
    // Its own drain ended and no other drain is live, so a pass would abandon
    // the queued leaf — but a new drain for the owner starts during the bind.
    jobs.finalize_job_run(&drain_run, JobRunState::Cancelled, Utc::now(), None)
        .expect("drain ended");
    let starter = runtime.clone();
    *owner.on_bind.lock().unwrap() = Some(Box::new(move || {
        starter
            .stores()
            .jobs()
            .insert_job_run(
                PULL_DRAIN_JOB,
                1,
                Utc::now(),
                Some(json!({ "destination": destination() })),
                None,
            )
            .expect("a drain starts");
    }));

    let entries = runtime.settle_pending_pulls();
    assert_eq!(
        outcomes(&entries),
        [(Some("task-1".into()), "awaiting_drain".into())]
    );
    assert_eq!(
        jobs.get_job_run(&leaf_of(&queued))
            .expect("read")
            .expect("queued leaf")
            .state,
        JobRunState::Pending,
        "the new drain can still launch it"
    );
    assert!(owner.settlements().is_empty());
}

fn refill_input(drain_run: &str, window_expired: bool) -> Value {
    json!({
        "run_id": drain_run,
        "destination": destination(),
        "window_expired": window_expired,
        "max_active_leaf_runs": 2,
        "poll_sleep_seconds": 30,
        "idle_sleep_seconds": 60,
    })
}

/// A hung owner is contacted once per pass. When the refill itself ran and
/// failed against it, the pass must not reconcile again on top: that would
/// send the same unanswered request twice per iteration.
#[test]
fn a_refill_that_failed_against_an_unreachable_owner_does_not_call_it_twice() {
    if isolated_pull_test(
        "adapter::engine_host::v2_host::pull::tests::settle::a_refill_that_failed_against_an_unreachable_owner_does_not_call_it_twice",
    ) {
        return;
    }
    let owner = Arc::new(Owner::default());
    let (_temp, runtime) = follower(&owner);
    let (drain_run, _template) = pull_drain(&runtime);
    *owner.pull_unreachable.lock().unwrap() = true;

    let pass = pull_refill(&runtime, "pull_refill", &refill_input(&drain_run, false))
        .expect("an unreachable owner does not fail the drain");
    assert_eq!(pass["admitted"], 0);
    assert!(
        pass["error"]
            .as_str()
            .is_some_and(|error| error.contains("unreachable")),
        "{pass}"
    );
    assert_eq!(pass["sleep_seconds"], 30);
    assert_eq!(pass["done"], false);
    assert_eq!(owner.tool_calls("orbit.drain.probe").len(), 1);
    assert_eq!(owner.tool_calls("orbit.task.pull").len(), 1);

    // The next pass retries the same unanswered request once, not twice.
    pull_refill(&runtime, "pull_refill", &refill_input(&drain_run, false)).expect("next pass");
    assert_eq!(owner.tool_calls("orbit.task.pull").len(), 2);
    assert_eq!(records(&runtime).len(), 1, "the same request ID, retried");
}

/// The drain reports a local store read that fails and tries again, like an
/// owner that is unreachable; it neither ends the drain nor reads the failed
/// count as "nothing left to settle".
#[test]
fn an_unreadable_admission_store_is_reported_and_retried_not_fatal() {
    if isolated_pull_test(
        "adapter::engine_host::v2_host::pull::tests::settle::an_unreadable_admission_store_is_reported_and_retried_not_fatal",
    ) {
        return;
    }
    let owner = Arc::new(Owner::default());
    let (_temp, runtime) = follower(&owner);
    let jobs = runtime.stores().jobs();
    let (drain_run, template) = pull_drain(&runtime);
    let peer = RoutedPullPeer {
        transport: owner.clone(),
    };
    let drain = PullDrain {
        jobs,
        peer: &peer,
        launcher: &AcknowledgingLauncher,
    };
    assert_eq!(
        drain.refill(&destination(), &template, 1).expect("admit"),
        1
    );
    rusqlite::Connection::open(runtime.global_root().join("orbit.db"))
        .expect("open orbit db")
        .execute(
            "UPDATE local_pull_admissions SET record_json='not json'",
            [],
        )
        .expect("corrupt the admission row");
    let probes = owner.tool_calls("orbit.drain.probe").len();

    // A closed window would end the drain if the unreadable count read as zero.
    let pass = pull_refill(&runtime, "pull_refill", &refill_input(&drain_run, true))
        .expect("a store read failure does not fail the drain");
    assert!(pass["error"].is_string(), "{pass}");
    assert_eq!(pass["done"], false);
    assert_eq!(pass["wait"], true);
    assert_eq!(pass["sleep_seconds"], 30);
    assert_eq!(pass["admitting"], false);
    assert_eq!(pass["admitted"], 0);
    assert_eq!(
        owner.tool_calls("orbit.drain.probe").len(),
        probes,
        "nothing is requested while the breaker cannot be read"
    );
}
