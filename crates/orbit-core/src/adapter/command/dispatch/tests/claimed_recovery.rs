//! A claimed leaf's final recovery reading its run's review and delivery
//! evidence between the nested `orbit`'s bridge and [`RunDispatch`] over the
//! broker's real socket, on a follower leaf admitted, created, bound and
//! launched through its durable pull admission, against the recording owner
//! route of the claimed-review fixture.
//!
//! Admitted as a security invariant the boundary cannot reach on a host
//! without a confined follower leaf: the read-only scope comes only from the
//! leaf's admission and run state, every neighbouring request stays refused
//! with its reason, and nothing refused reaches the owner.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use chrono::Utc;
use orbit_common::process::ancestry::process_start_key;
use orbit_engine::{PluginBrokerHandle, PluginBrokerRun};
use orbit_store::contracts::{
    AdmissionReceipt, AdmissionRequest, AdmissionRunContext, AdmissionShipContract,
    AdmissionTaskSummary, ClaimEvidence, ClaimMutation, DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA,
    ExecutionClaim, ExecutionClaimPhase, ExecutionLocation, LocalPullMutation, PullDestination,
};
use orbit_tools::plugin::BrokeredCaller;
use orbit_types::policy::ResolvedFsProfile;
use orbit_types::tool::WorkerInvocation;
use orbit_types::workflow::{
    ActivityToolDenyPolicy, FINAL_RECOVERY_ACTIVITY, FinalRecoveryCheckpoint,
    FinalRecoveryDecision, FinalRecoveryKey, JobRunState, PipelineState, REVIEW_BASELINE_ARTIFACT,
    REVIEW_EVIDENCE_HOLD_ARTIFACT, REVIEW_GATE_ARTIFACT, REVIEW_MANIFEST_ARTIFACT,
    REVIEW_REPORT_ARTIFACT, REVIEW_REPORT_HISTORY_ARTIFACT,
};
use serde_json::{Value, json};
use tempfile::TempDir;

use super::super::brokered::RunDispatch;
use super::super::claimed_owner::{ClaimedOwnerRoute, bridge_through};
use super::super::execute::ToolEntryPoint;
use super::claimed_review::{FOLLOWER, GET, OWNER, Owner, PUT, TASK};
use crate::OrbitRuntime;
use crate::runtime::plugin::broker::{
    BrokerDispatch, BrokerRequest, EntryPoint, PeerAnchor, PluginBroker,
};

const SHOW: &str = "orbit.task.show";
const OWNER_WORKSPACE: &str = "ws_owner";

/// The gate's artifacts final recovery reads, with the bytes the owner holds.
const REVIEW_EVIDENCE: [(&str, &str); 5] = [
    (REVIEW_MANIFEST_ARTIFACT, r#"{"attempt_id":"attempt-1"}"#),
    (REVIEW_REPORT_ARTIFACT, r#"{"verdict":"incomplete"}"#),
    (REVIEW_REPORT_HISTORY_ARTIFACT, r#"{"revisions":[]}"#),
    (REVIEW_GATE_ARTIFACT, r#"{"outcome":"blocked"}"#),
    (REVIEW_BASELINE_ARTIFACT, r#"{"claims":[]}"#),
];

/// The tools `final_recovery.yaml` withholds that a claimed worker's broker
/// would otherwise carry.
fn recovery_policy() -> ActivityToolDenyPolicy {
    ActivityToolDenyPolicy {
        activity: FINAL_RECOVERY_ACTIVITY.into(),
        disallow_list: vec![
            "orbit.task.add".into(),
            "orbit.task.update".into(),
            PUT.into(),
        ],
    }
}

/// A claimed leaf on the follower: admitted for the owner's claim on
/// [`TASK`], created, bound and launched through its durable admission, its
/// run running, with final recovery admitted for its failed step.
struct Leaf {
    _root: TempDir,
    global_root: PathBuf,
    worktree: PathBuf,
    owner: Arc<Owner>,
    /// The follower's runtime without the claim's binding, for host records.
    host: OrbitRuntime,
    /// The follower's runtime bound to the claim, as its step runner is.
    runtime: OrbitRuntime,
    binding: WorkerInvocation,
    destination: PullDestination,
    request_id: String,
    leaf: String,
    drain: String,
}

impl Leaf {
    fn admit() -> Self {
        // Short, for the broker's socket path.
        let root = tempfile::Builder::new()
            .prefix("ocf")
            .tempdir_in("/tmp")
            .expect("short fixture root");
        let global_root = root.path().join("global");
        let worktree = root.path().join("repo");
        std::fs::create_dir_all(&global_root).unwrap();
        std::fs::create_dir_all(worktree.join(".orbit/tmp")).unwrap();
        let worktree = worktree.canonicalize().unwrap();
        let host = OrbitRuntime::from_roots(&global_root, &worktree.join(".orbit"))
            .unwrap()
            .with_automation_machine_identity(Some(FOLLOWER.into()));
        let jobs = host.stores().jobs();
        let drain = jobs
            .insert_job_run(
                "workspace_pull_pipeline",
                1,
                Utc::now(),
                Some(json!({})),
                None,
            )
            .unwrap();
        host.write_run_state(
            &drain.run_id,
            &PipelineState::new(drain.run_id.clone(), drain.job_id.clone(), json!({})),
        )
        .unwrap();
        jobs.mark_job_run_running(&drain.run_id, Utc::now(), std::process::id())
            .unwrap();
        let destination = PullDestination {
            owner_machine_id: OWNER.into(),
            owner_workspace_id: OWNER_WORKSPACE.into(),
            selector: format!("{OWNER}/{OWNER_WORKSPACE}"),
            execution_machine_id: FOLLOWER.into(),
        };
        let request = AdmissionRequest {
            request_id: "request-1".into(),
            caller_version: "fixture".into(),
            caller_schema: DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA,
            caller_fingerprint: None,
            caller_before_pr: true,
            review_gate: true,
            run_context: AdmissionRunContext {
                run_id: drain.run_id.clone(),
                job_name: drain.job_id.clone(),
                machine_name: None,
            },
            ship: AdmissionShipContract {
                mode: "pr".into(),
                base_branch: "main".into(),
                landing_branch: "main".into(),
                before_pr: false,
                completion: "review".into(),
                authorization_reference: None,
                review: None,
            },
            crews: None,
            os: None,
        };
        let record = jobs
            .allocate_pull_request(&destination, &request, 10)
            .unwrap()
            .expect("a pull slot");
        let request_id = record.request.request_id.clone();
        let executed_on = ExecutionLocation {
            machine_id: FOLLOWER.into(),
            machine_name: None,
        };
        let receipt = AdmissionReceipt {
            schema_version: 1,
            request: record.request.clone(),
            machine_id: FOLLOWER.into(),
            claim: Some(ExecutionClaim {
                claim_id: "claim-1".into(),
                task_id: TASK.into(),
                request_id: request_id.clone(),
                executed_on: executed_on.clone(),
                run_context: record.request.run_context.clone(),
                footprint: vec![],
                reservation_id: "reservation-1".into(),
                reservation_expires_at: (Utc::now() + chrono::Duration::hours(1)).to_rfc3339(),
                repair: None,
                phase: ExecutionClaimPhase::Claimed,
            }),
            task: Some(AdmissionTaskSummary {
                id: TASK.into(),
                title: "Claimed work".into(),
                complexity: None,
                crew: None,
                context_files: vec![],
                resume_candidate: None,
            }),
            invalid_candidates: vec![],
            deferred_conflicts: vec![],
            crew_unavailable: vec![],
            os_unavailable: vec![],
            queue_depth: 0,
        };
        let mut record = jobs
            .mutate_local_pull(
                &destination,
                &request_id,
                &LocalPullMutation::Receive(Box::new(receipt)),
            )
            .unwrap();
        for mutation in [
            LocalPullMutation::CreateLeaf,
            LocalPullMutation::Bound,
            LocalPullMutation::LaunchIntent,
        ] {
            record = jobs
                .mutate_local_pull(&destination, &request_id, &mutation)
                .unwrap();
        }
        let leaf = record.leaf_run_id.clone().expect("the leaf run");
        let run = jobs.get_job_run(&leaf).unwrap().expect("the leaf record");
        host.write_run_state(
            &leaf,
            &PipelineState::new(leaf.clone(), run.job_id.clone(), json!({})),
        )
        .unwrap();
        jobs.mark_job_run_running(&leaf, Utc::now(), std::process::id())
            .unwrap();
        jobs.mutate_local_pull(&destination, &request_id, &LocalPullMutation::Launched)
            .unwrap();

        let binding = WorkerInvocation {
            owner_machine_id: OWNER.into(),
            owner_workspace_id: OWNER_WORKSPACE.into(),
            owner_destination: destination.selector.clone(),
            task_id: TASK.into(),
            claim_id: "claim-1".into(),
            execution: executed_on,
            bound_run_id: leaf.clone(),
        };
        let owner = Arc::new(Owner::default());
        for (path, bytes) in REVIEW_EVIDENCE {
            owner.hold(path, bytes.as_bytes());
        }
        let runtime = host
            .clone()
            .with_worker_invocation(binding.clone(), owner.clone())
            .unwrap();
        let fixture = Self {
            _root: root,
            global_root,
            worktree,
            owner,
            host,
            runtime,
            binding,
            destination,
            request_id,
            leaf,
            drain: drain.run_id,
        };
        fixture.set_recovery(None);
        fixture
    }

    /// Record the leaf's final recovery as admitted for its failed step, the
    /// way admission writes it, and decided when `decision` is given.
    fn set_recovery(&self, decision: Option<FinalRecoveryDecision>) {
        let leaf = self.leaf.clone();
        self.host
            .stores()
            .jobs()
            .update_run_state(&self.leaf, &mut |_, state| {
                state.final_recovery = Some(FinalRecoveryCheckpoint {
                    key: FinalRecoveryKey {
                        run_id: leaf.clone(),
                        attempt: 1,
                    },
                    failed_step_id: "review".into(),
                    task_id: TASK.into(),
                    observed: None,
                    base_ref: Some("main".into()),
                    admitted_at: Utc::now(),
                    decision: decision.clone(),
                    repair_commit: None,
                    outcome: None,
                });
                Ok(())
            })
            .unwrap();
    }

    fn advance(&self, mutation: LocalPullMutation) {
        self.host
            .stores()
            .jobs()
            .mutate_local_pull(&self.destination, &self.request_id, &mutation)
            .unwrap();
    }

    /// The broker run the step runner builds for `activity` of the leaf.
    fn run(&self, activity: &str) -> PluginBrokerRun {
        PluginBrokerRun {
            run_id: format!("{}-{activity}", self.leaf),
            job_run_id: Some(self.leaf.clone()),
            task_id: Some(TASK.into()),
            activity_name: activity.into(),
            agent_name: Some("codex".into()),
            model_name: None,
            workspace: None,
            allowed_tools: Vec::new(),
            tool_deny_policy: Some(if activity == FINAL_RECOVERY_ACTIVITY {
                recovery_policy()
            } else {
                ActivityToolDenyPolicy {
                    activity: activity.into(),
                    disallow_list: vec!["orbit.task.update".into()],
                }
            }),
            caller: BrokeredCaller {
                worktree: self.worktree.clone(),
                fs_profile: ResolvedFsProfile {
                    name: "claimed-recovery".into(),
                    read: vec!["/**".into()],
                    modify: vec![format!("{}/**", self.worktree.display())],
                },
                proc_allowed_programs: Vec::new(),
                proc_disallowed_programs: None,
            },
        }
    }

    fn serve_run(&self, runtime: &OrbitRuntime, run: PluginBrokerRun) -> PluginBroker {
        let dispatch = Arc::new(RunDispatch::new(runtime.clone(), run.clone()));
        let broker = PluginBroker::start(&self.global_root, &run.run_id, dispatch).unwrap();
        broker.bind_anchor(PeerAnchor::Ancestor(
            process_start_key(std::process::id()).unwrap(),
        ));
        broker
    }

    fn serve(&self, activity: &str) -> PluginBroker {
        self.serve_run(&self.runtime, self.run(activity))
    }

    /// The nested `orbit`'s call, as its CLI makes it, through `broker`.
    fn call(
        &self,
        socket: &Path,
        binding: &WorkerInvocation,
        name: &str,
        input: Value,
    ) -> Option<ClaimedOwnerRoute> {
        bridge_through(
            Some(socket),
            &self.global_root,
            Some(binding),
            Some(FOLLOWER),
            name,
            &input,
            &self.worktree,
            &self.worktree,
            ToolEntryPoint::Cli,
        )
    }

    fn forwarded(&self, broker: &PluginBroker, name: &str, input: Value) -> Result<Value, String> {
        match self.call(broker.socket_path(), &self.binding, name, input) {
            Some(ClaimedOwnerRoute::Forwarded(result)) => result.map_err(|error| error.to_string()),
            other => panic!("{name} reaches the broker: {other:?}"),
        }
    }

    /// A request sent to the broker as the sandboxed agent itself can send
    /// it, bypassing whatever the nested `orbit` would prepare.
    fn raw(&self, broker: &PluginBroker, name: &str, input: Value) -> Result<Value, String> {
        crate::runtime::plugin::broker::forward_call_with_status(
            broker.socket_path(),
            name,
            input,
            &self.worktree,
            None,
            "cli",
        )
        .map_err(|error| match error {
            crate::runtime::plugin::broker::ForwardCallError::BrokerAudit(error)
            | crate::runtime::plugin::broker::ForwardCallError::CallerAudit(error) => {
                error.to_string()
            }
        })
    }

    fn delivery(&self, broker: &PluginBroker, run_id: Option<&str>) -> Result<Value, String> {
        let mut input = json!({"id": TASK, "field": "delivery"});
        if let Some(run_id) = run_id {
            input["run_id"] = json!(run_id);
        }
        self.forwarded(broker, SHOW, input)
    }

    fn owner_untouched(&self, case: &str) {
        assert!(
            self.owner.calls.lock().unwrap().is_empty(),
            "{case}: nothing refused reaches the owner"
        );
    }
}

fn refused_with(result: Result<Value, String>, reason: &str, case: &str) {
    let error = result.expect_err(&format!("{case} is refused"));
    assert!(error.contains(reason), "{case}: {error}");
}

#[test]
fn a_claimed_leafs_final_recovery_reads_its_review_evidence_and_delivery_through_the_broker() {
    let fixture = Leaf::admit();
    let broker = fixture.serve(FINAL_RECOVERY_ACTIVITY);

    for (path, bytes) in REVIEW_EVIDENCE {
        let read = fixture
            .forwarded(
                &broker,
                GET,
                json!({"id": TASK, "path": path, "model": "codex"}),
            )
            .unwrap_or_else(|error| panic!("{path} is readable: {error}"));
        assert_eq!(
            read["content"], bytes,
            "{path} crosses with its exact bytes"
        );
    }
    assert_eq!(
        fixture.owner.reads(),
        REVIEW_EVIDENCE.map(|(path, _)| path.to_string()),
        "each read reaches the owner once, under its own name"
    );
    for (name, input, session) in fixture.owner.calls.lock().unwrap().iter() {
        assert_eq!(name, GET);
        assert_eq!(input["id"], TASK, "{input}");
        assert_eq!(
            session.worker_invocation.as_ref(),
            Some(&fixture.binding),
            "{name} rides the claim's binding"
        );
    }
    // An artifact the gate never wrote is the owner's not-found: missing
    // evidence, not a refusal.
    fixture.owner.forget(REVIEW_BASELINE_ARTIFACT);
    let absent = fixture
        .forwarded(
            &broker,
            GET,
            json!({"id": TASK, "path": REVIEW_BASELINE_ARTIFACT}),
        )
        .unwrap_err();
    assert!(absent.contains("not found"), "{absent}");

    // The delivery view of the leaf, named or defaulted, comes from the
    // follower's record of the leaf; the owner has none to forward to.
    let calls_before = fixture.owner.calls.lock().unwrap().len();
    for run_id in [Some(fixture.leaf.as_str()), None] {
        let observation = fixture.delivery(&broker, run_id).unwrap();
        assert_eq!(
            observation["run_id"],
            fixture.leaf.as_str(),
            "{observation}"
        );
        assert_eq!(observation["task_id"], TASK, "{observation}");
        assert_eq!(observation["run_state"], "running", "{observation}");
        assert!(
            observation.get("delivery_status").is_some(),
            "{observation}"
        );
    }
    assert_eq!(
        fixture.owner.calls.lock().unwrap().len(),
        calls_before,
        "no delivery read is forwarded to the owner"
    );

    let rows = fixture
        .runtime
        .list_audit_events(None, Some(GET.to_string()), None, None, 20)
        .unwrap();
    assert!(
        rows.iter().all(|row| row.brokered
            && row.task_id.as_deref() == Some(TASK)
            && row.activity_id.as_deref() == Some(FINAL_RECOVERY_ACTIVITY)),
        "the broker audits each read for the recovering activity: {rows:?}"
    );
}

/// Neighbouring rejection 1: writes of a gate artifact stay the reviewer's,
/// whether the activity's own policy or the recovery scope refuses them.
#[test]
fn final_recovery_attaches_no_review_artifact() {
    let fixture = Leaf::admit();
    let forged = {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode(br#"{"verdict":"accept"}"#)
    };

    let broker = fixture.serve(FINAL_RECOVERY_ACTIVITY);
    let policy = fixture.raw(
        &broker,
        PUT,
        json!({"id": TASK, "path": REVIEW_REPORT_ARTIFACT, "content_base64": forged}),
    );
    refused_with(policy, PUT, "the activity's own policy");

    // A recovery activity whose policy grants the put still writes nothing.
    let mut granted = fixture.run(FINAL_RECOVERY_ACTIVITY);
    granted.tool_deny_policy = Some(ActivityToolDenyPolicy {
        activity: FINAL_RECOVERY_ACTIVITY.into(),
        disallow_list: vec!["orbit.task.update".into()],
    });
    let broker = fixture.serve_run(&fixture.runtime, granted);
    for path in [
        REVIEW_REPORT_ARTIFACT,
        REVIEW_GATE_ARTIFACT,
        REVIEW_MANIFEST_ARTIFACT,
        REVIEW_EVIDENCE_HOLD_ARTIFACT,
        "review-certificate.json",
    ] {
        refused_with(
            fixture.raw(
                &broker,
                PUT,
                json!({"id": TASK, "path": path, "content_base64": forged}),
            ),
            "review_write_refused",
            path,
        );
    }
    fixture.owner_untouched("a review artifact put");
}

/// Neighbouring rejection 2: another task, a request that names another
/// workspace, and a worker bound to another workspace's claim.
#[test]
fn final_recovery_reads_nothing_of_another_task_or_workspace() {
    let fixture = Leaf::admit();
    let broker = fixture.serve(FINAL_RECOVERY_ACTIVITY);
    for (case, name, input) in [
        (
            "another task's review artifact",
            GET,
            json!({"id": "TSO-2", "path": REVIEW_REPORT_ARTIFACT}),
        ),
        (
            "another task's delivery",
            SHOW,
            json!({"id": "TSO-2", "field": "delivery"}),
        ),
    ] {
        refused_with(
            fixture.forwarded(&broker, name, input),
            "a task other than the claimed task",
            case,
        );
    }
    let mut other_task = fixture.run(FINAL_RECOVERY_ACTIVITY);
    other_task.task_id = Some("TSO-2".into());
    let other = fixture.serve_run(&fixture.runtime, other_task);
    refused_with(
        fixture.forwarded(
            &other,
            GET,
            json!({"id": TASK, "path": REVIEW_REPORT_ARTIFACT}),
        ),
        "the run's task is not the claimed task",
        "a run of another task",
    );

    // A request naming another workspace never leaves the nested `orbit`,
    // and the broker takes no workspace field from a raw request.
    match fixture.call(
        broker.socket_path(),
        &fixture.binding,
        GET,
        json!({"id": TASK, "path": REVIEW_REPORT_ARTIFACT, "workspace": "hm_owner/ws_other"}),
    ) {
        Some(ClaimedOwnerRoute::Refused(error)) => assert!(
            error.to_string().contains("workspace binding mismatch"),
            "{error}"
        ),
        other => panic!("another workspace is refused before the broker: {other:?}"),
    }
    let serving = RunDispatch::new(
        fixture.runtime.clone(),
        fixture.run(FINAL_RECOVERY_ACTIVITY),
    );
    let refused = serving.call(
        BrokerRequest {
            tool: GET.into(),
            input: json!({"id": TASK, "path": REVIEW_REPORT_ARTIFACT, "workspace": "hm_owner/ws_other"}),
            cwd: fixture.worktree.clone(),
            workspace: None,
            entry_point: EntryPoint::Cli,
            dry_run: false,
        },
        std::process::id(),
        Arc::new(AtomicBool::new(false)),
    );
    assert!(
        refused
            .unwrap_err()
            .to_string()
            .contains("does not accept `workspace`")
    );

    // A worker bound to the same task through another workspace's claim
    // does not match the leaf's admission.
    let foreign = WorkerInvocation {
        owner_workspace_id: "ws_other".into(),
        owner_destination: format!("{OWNER}/ws_other"),
        ..fixture.binding.clone()
    };
    let runtime = fixture
        .host
        .clone()
        .with_worker_invocation(foreign.clone(), fixture.owner.clone())
        .unwrap();
    let broker = fixture.serve_run(&runtime, fixture.run(FINAL_RECOVERY_ACTIVITY));
    for (name, input) in [
        (GET, json!({"id": TASK, "path": REVIEW_REPORT_ARTIFACT})),
        (SHOW, json!({"id": TASK, "field": "delivery"})),
    ] {
        let result = match fixture.call(broker.socket_path(), &foreign, name, input) {
            Some(ClaimedOwnerRoute::Forwarded(result)) => result.map_err(|error| error.to_string()),
            other => panic!("{name} reaches the broker: {other:?}"),
        };
        refused_with(result, "claim_unbound", "another workspace's binding");
    }
    fixture.owner_untouched("another task or workspace");
}

/// Neighbouring rejection 3: a run that is not the bound leaf, and a leaf
/// whose recovery has decided, whose run has ended, or whose claim is
/// settling.
#[test]
fn final_recovery_reads_only_while_its_leaf_and_recovery_are_live() {
    let read = |fixture: &Leaf, broker: &PluginBroker| {
        fixture.forwarded(
            broker,
            GET,
            json!({"id": TASK, "path": REVIEW_REPORT_ARTIFACT}),
        )
    };
    let fixture = Leaf::admit();
    let mut not_leaf = fixture.run(FINAL_RECOVERY_ACTIVITY);
    not_leaf.job_run_id = Some(fixture.drain.clone());
    let broker = fixture.serve_run(&fixture.runtime, not_leaf);
    refused_with(read(&fixture, &broker), "not_claimed_leaf", "another run");
    refused_with(
        fixture.delivery(&broker, None),
        "not_claimed_leaf",
        "another run's delivery",
    );

    let broker = fixture.serve(FINAL_RECOVERY_ACTIVITY);
    fixture.set_recovery(Some(FinalRecoveryDecision::Escalate {
        diagnosis: "decided".into(),
        human_action: "look".into(),
    }));
    refused_with(
        read(&fixture, &broker),
        "final_recovery_stale",
        "a decided recovery",
    );
    refused_with(
        fixture.delivery(&broker, None),
        "final_recovery_stale",
        "a decided recovery's delivery",
    );
    fixture.set_recovery(None);
    read(&fixture, &broker).expect("an undecided recovery reads");
    fixture
        .host
        .stores()
        .jobs()
        .finalize_job_run(&fixture.leaf, JobRunState::Failed, Utc::now(), None)
        .unwrap();
    refused_with(
        read(&fixture, &broker),
        "final_recovery_stale",
        "an ended run",
    );

    let fixture = Leaf::admit();
    let broker = fixture.serve(FINAL_RECOVERY_ACTIVITY);
    fixture.advance(LocalPullMutation::Settle(Box::new(ClaimMutation::Release(
        ClaimEvidence {
            summary: Some("The executor gave the claim back.".into()),
            ..ClaimEvidence::default()
        },
    ))));
    refused_with(
        read(&fixture, &broker),
        "claim_not_live",
        "a settling claim",
    );
    refused_with(
        fixture.delivery(&broker, None),
        "claim_not_live",
        "a settling claim's delivery",
    );
    fixture.owner_untouched("a stale leaf or recovery");
}

/// Neighbouring rejection 4: a `review-*` artifact outside the gate's named
/// evidence, and a spelling that only normalises to one of those names.
#[test]
fn final_recovery_reads_only_the_named_review_artifacts_spelled_exactly() {
    let fixture = Leaf::admit();
    let broker = fixture.serve(FINAL_RECOVERY_ACTIVITY);
    let mut paths = vec![
        REVIEW_EVIDENCE_HOLD_ARTIFACT.to_string(),
        "review-certificate.json".to_string(),
        "review-evidence-ci.json".to_string(),
    ];
    for name in [REVIEW_GATE_ARTIFACT, REVIEW_REPORT_ARTIFACT] {
        paths.extend([
            format!(" {name}"),
            format!("{name}\n"),
            format!("./{name}"),
            name.to_ascii_uppercase(),
            format!("R{}", &name[1..]),
        ]);
    }
    for path in paths {
        refused_with(
            fixture.raw(&broker, GET, json!({"id": TASK, "path": path})),
            "review_read_refused",
            &format!("{path:?}"),
        );
    }
    fixture.owner_untouched("an unnamed or respelled review artifact");

    // Outside the gate's namespace, an artifact is the claimed task's
    // ordinary read, which this scope neither widens nor narrows.
    fixture.owner.hold("notes.md", b"notes");
    fixture
        .forwarded(&broker, GET, json!({"id": TASK, "path": "notes.md"}))
        .unwrap();
}

/// Neighbouring rejection 5: the delivery view of any run but the leaf.
#[test]
fn a_claimed_worker_reads_the_delivery_of_its_own_leaf_only() {
    let fixture = Leaf::admit();
    for activity in [FINAL_RECOVERY_ACTIVITY, "agent_implement"] {
        let broker = fixture.serve(activity);
        for run_id in [fixture.drain.as_str(), "jrun-elsewhere", ""] {
            refused_with(
                fixture.delivery(&broker, Some(run_id)),
                "delivery_run_refused",
                &format!("{activity} reading run {run_id:?}"),
            );
        }
        let combined = fixture
            .forwarded(
                &broker,
                SHOW,
                json!({"id": TASK, "fields": ["delivery", "status"]}),
            )
            .unwrap_err();
        assert!(combined.contains("cannot be combined"), "{combined}");
        // The owner decodes a JSON-encoded array in a string, so this is the
        // delivery view of another run, not a field this scope may forward.
        refused_with(
            fixture.forwarded(
                &broker,
                SHOW,
                json!({"id": TASK, "fields": "[\"delivery\"]", "run_id": "jrun-elsewhere"}),
            ),
            "delivery_run_refused",
            &format!("{activity} encoded delivery field"),
        );
        let own = fixture.delivery(&broker, Some(&fixture.leaf)).unwrap();
        assert_eq!(own["run_id"], fixture.leaf.as_str(), "{activity}: {own}");
    }
    fixture.owner_untouched("another run's delivery");
}
