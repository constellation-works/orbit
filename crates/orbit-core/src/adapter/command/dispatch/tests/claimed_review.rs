//! A claimed reviewer's artifact calls between the nested `orbit`'s bridge
//! and [`RunDispatch`] over the broker's real socket, against an owner route
//! that records what reaches it and can lose an answer after committing. The
//! fixture is shared with the claimed-owner bridge's tests.
//!
//! Admitted as a security invariant and a fault injection the boundary test
//! (`orbit-cli` `claimed_review_bridge_sandbox`) cannot reach on a host
//! without user namespaces: the broker's scope comes only from the run's
//! records, a refused request never reaches the owner, and a lost answer is
//! retried without a second effect. The client is this test process,
//! anchored by ancestry, as in the plugin broker's own tests.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::process::ancestry::process_start_key;
use orbit_engine::{PluginBrokerHandle, PluginBrokerRun};
use orbit_store::contracts::{ReviewInvocationRecord, ReviewReserveRequest};
use orbit_tools::OwnerCoordinator;
use orbit_tools::plugin::BrokeredCaller;
use orbit_types::policy::ResolvedFsProfile;
use orbit_types::task::ExecutionLocation;
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::tool::{ToolSessionContext, WorkerInvocation};
use orbit_types::workflow::automation::SourceRevision;
use orbit_types::workflow::{
    ActivityToolDenyPolicy, REVIEW_EVIDENCE_HOLD_ARTIFACT, REVIEW_MANIFEST_ARTIFACT,
    REVIEW_REPORT_ARTIFACT, REVIEW_REPORT_HISTORY_ARTIFACT, ReviewBudget, ReviewReservation,
    ReviewerInvocationEvent,
};
use serde_json::{Value, json};
use tempfile::TempDir;

use super::super::brokered::RunDispatch;
use super::super::claimed_owner::{ClaimedOwnerRoute, bridge_through};
use super::super::execute::ToolEntryPoint;
use crate::OrbitRuntime;
use crate::runtime::plugin::broker::{PeerAnchor, PluginBroker};

pub(super) const FOLLOWER: &str = "hm_follower";
const OWNER: &str = "hm_owner";
pub(super) const TASK: &str = "TSO-1";
const LEAF: &str = "jrun-leaf-1";
const LINEAGE: &str = "lineage-1";
pub(super) const GET: &str = "orbit.task.artifact.get";
pub(super) const PUT: &str = "orbit.task.artifact.put";

/// The owner's side of the claim route: answers a read from `artifacts`, any
/// call other than an artifact call with what it received, records every
/// call, and can drop one put's answer after accepting it.
#[derive(Default)]
pub(super) struct Owner {
    artifacts: Mutex<BTreeMap<String, Vec<u8>>>,
    pub(super) calls: Mutex<Vec<(String, Value, ToolSessionContext)>>,
    lose_next_put: AtomicBool,
}

impl OwnerCoordinator for Owner {
    fn call(
        &self,
        name: &str,
        input: Value,
        session: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        self.calls
            .lock()
            .unwrap()
            .push((name.to_string(), input.clone(), session));
        if name == PUT {
            if self.lose_next_put.swap(false, Ordering::SeqCst) {
                return Err(OrbitError::OutcomeUnknown {
                    mcp_call_id: "lost".into(),
                    message: "the owner session ended before answering".into(),
                });
            }
            return Ok(json!({"id": input["id"], "updated": true}));
        }
        if name != GET {
            return Ok(json!({"tool": name, "input": input}));
        }
        let path = input["path"].as_str().unwrap_or_default();
        let Some(bytes) = self.artifacts.lock().unwrap().get(path).cloned() else {
            return Err(OrbitError::not_found(
                orbit_common::NotFoundKind::Artifact,
                format!("{TASK}/{path}"),
            ));
        };
        Ok(json!({
            "id": input["id"], "path": input["path"], "media_type": "application/json",
            "size": bytes.len(), "presentation": "text", "encoding": "utf-8",
            "content": String::from_utf8(bytes).unwrap(),
        }))
    }
}

impl Owner {
    pub(super) fn hold(&self, path: &str, bytes: &[u8]) {
        self.artifacts
            .lock()
            .unwrap()
            .insert(path.to_string(), bytes.to_vec());
    }

    fn reads(&self) -> Vec<String> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(name, ..)| name == GET)
            .map(|(_, input, _)| input["path"].as_str().unwrap_or_default().to_string())
            .collect()
    }

    pub(super) fn puts(&self) -> Vec<Value> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(name, ..)| name == PUT)
            .map(|(_, input, _)| input.clone())
            .collect()
    }
}

pub(super) struct Fixture {
    _root: TempDir,
    pub(super) global_root: PathBuf,
    pub(super) worktree: PathBuf,
    pub(super) owner: Arc<Owner>,
    pub(super) runtime: OrbitRuntime,
    attempt_id: String,
    pub(super) binding: WorkerInvocation,
}

impl Fixture {
    /// A follower leaf bound to its claim, with the attempt admitted and its
    /// reviewer running in the leaf.
    pub(super) fn new() -> Self {
        // Short, for the broker's socket path.
        let root = tempfile::Builder::new()
            .prefix("ocr")
            .tempdir_in("/tmp")
            .expect("short fixture root");
        let global_root = root.path().join("global");
        let worktree = root.path().join("repo");
        std::fs::create_dir_all(&global_root).unwrap();
        std::fs::create_dir_all(worktree.join(".orbit/tmp")).unwrap();
        let worktree = worktree.canonicalize().unwrap();
        let binding = WorkerInvocation {
            owner_machine_id: OWNER.into(),
            owner_workspace_id: "ws_owner".into(),
            owner_destination: format!("{OWNER}/ws_owner"),
            task_id: TASK.into(),
            claim_id: "claim-1".into(),
            execution: ExecutionLocation {
                machine_id: FOLLOWER.into(),
                machine_name: None,
            },
            bound_run_id: LEAF.into(),
        };
        let owner = Arc::new(Owner::default());
        let runtime = OrbitRuntime::from_roots(&global_root, &worktree.join(".orbit"))
            .unwrap()
            .with_automation_machine_identity(Some(FOLLOWER.into()))
            .with_worker_invocation(binding.clone(), owner.clone())
            .unwrap();
        let workspace = runtime.workspace_id().unwrap();
        let store = runtime.review_store().unwrap();
        let (reservation, _) = store
            .review_reserve(
                &workspace,
                &ReviewReserveRequest {
                    lineage_key: LINEAGE,
                    task_ids: &[TASK.to_string()],
                    run_id: LEAF,
                    task_meaning_digest: "digest",
                    candidate: &SourceRevision {
                        commit: "a".repeat(40),
                        tree: "b".repeat(40),
                    },
                    budget: ReviewBudget::default(),
                    now: Utc::now(),
                },
            )
            .unwrap();
        let ReviewReservation::Reserved { attempt } = reservation else {
            panic!("a fresh lineage reserves: {reservation:?}");
        };
        let fixture = Self {
            _root: root,
            global_root,
            worktree,
            owner,
            runtime,
            attempt_id: attempt.attempt_id,
            binding,
        };
        fixture.reviewer(ReviewerInvocationEvent::Started {
            timeout_seconds: 1800,
        });
        fixture.pin_manifest(&fixture.attempt_id.clone());
        fixture
    }

    fn reviewer(&self, event: ReviewerInvocationEvent) {
        self.reviewer_at(event, Utc::now());
    }

    fn reviewer_at(&self, event: ReviewerInvocationEvent, now: chrono::DateTime<Utc>) {
        self.runtime
            .review_store()
            .unwrap()
            .review_record_invocation(
                &self.runtime.workspace_id().unwrap(),
                &ReviewInvocationRecord {
                    lineage_key: LINEAGE,
                    attempt_id: &self.attempt_id,
                    run_id: LEAF,
                    event,
                    now,
                },
            )
            .unwrap();
    }

    /// What the owner holds as the pinned manifest: the fields the broker
    /// checks, for `attempt`.
    fn pin_manifest(&self, attempt: &str) -> Vec<u8> {
        let manifest = json!({
            "schema_version": 1, "attempt_id": attempt, "lineage_key": LINEAGE,
            "task_ids": [TASK], "task_digests": {TASK: "digest"},
            "task_meaning_digest": "digest", "repository": "owner/repository",
            "base": {"commit": "c".repeat(40), "tree": "d".repeat(40)},
            "candidate": {"commit": "a".repeat(40), "tree": "b".repeat(40)},
            "implementation_commits": [], "implementer_summaries": {},
            "reviewer_crew": "sol", "contract_version": 1, "policy_version": 1,
            "budget": {"minutes": 30}, "remaining": {"seconds": 1800},
            "issued_at": Utc::now(),
        });
        let bytes = serde_json::to_vec_pretty(&manifest).unwrap();
        self.owner.hold(REVIEW_MANIFEST_ARTIFACT, &bytes);
        bytes
    }

    pub(super) fn run(&self, activity: &str) -> PluginBrokerRun {
        PluginBrokerRun {
            run_id: format!("{LEAF}-{activity}"),
            job_run_id: Some(LEAF.into()),
            task_id: Some(TASK.into()),
            activity_name: activity.into(),
            agent_name: Some("codex".into()),
            model_name: None,
            workspace: None,
            allowed_tools: Vec::new(),
            tool_deny_policy: Some(ActivityToolDenyPolicy {
                activity: activity.into(),
                disallow_list: vec!["orbit.agent.invoke".into(), "orbit.task.update".into()],
            }),
            caller: BrokeredCaller {
                worktree: self.worktree.clone(),
                fs_profile: ResolvedFsProfile {
                    name: "claimed-review".into(),
                    read: vec!["/**".into()],
                    modify: vec![format!("{}/**", self.worktree.display())],
                },
                proc_allowed_programs: Vec::new(),
                proc_disallowed_programs: None,
            },
        }
    }

    pub(super) fn serve(&self, activity: &str) -> PluginBroker {
        let run = self.run(activity);
        let dispatch = Arc::new(RunDispatch::new(self.runtime.clone(), run.clone()));
        let broker = PluginBroker::start(&self.global_root, &run.run_id, dispatch).unwrap();
        broker.bind_anchor(PeerAnchor::Ancestor(
            process_start_key(std::process::id()).unwrap(),
        ));
        broker
    }

    /// The nested `orbit`'s call, as its CLI makes it.
    pub(super) fn call(
        &self,
        socket: Option<&Path>,
        name: &str,
        input: Value,
    ) -> Option<ClaimedOwnerRoute> {
        bridge_through(
            socket,
            &self.global_root,
            Some(&self.binding),
            Some(FOLLOWER),
            name,
            &input,
            &self.worktree,
            &self.worktree,
            ToolEntryPoint::Cli,
        )
    }

    pub(super) fn forwarded(
        &self,
        broker: &PluginBroker,
        name: &str,
        input: Value,
    ) -> Result<Value, String> {
        match self.call(Some(broker.socket_path()), name, input) {
            Some(ClaimedOwnerRoute::Forwarded(result)) => result.map_err(|error| error.to_string()),
            other => panic!("{name} reaches the broker: {other:?}"),
        }
    }

    pub(super) fn report(&self, name: &str, attempt: &str) -> (PathBuf, Vec<u8>) {
        let bytes = serde_json::to_vec(&json!({
            "schema_version": 1, "attempt_id": attempt, "verdict": "incomplete",
            "summary": "Fixture review.", "findings": [], "validation": [],
            "escalation": "fixture only",
        }))
        .unwrap();
        let path = self.worktree.join(".orbit/tmp").join(name);
        std::fs::write(&path, &bytes).unwrap();
        (path, bytes)
    }
}

#[test]
fn the_running_reviewer_reads_its_manifest_and_attaches_its_report_through_the_broker() {
    let fixture = Fixture::new();
    let broker = fixture.serve("agent_review_repair");
    let pinned = fixture.owner.artifacts.lock().unwrap()[REVIEW_MANIFEST_ARTIFACT].clone();

    let read = fixture
        .forwarded(
            &broker,
            GET,
            json!({"id": TASK, "path": REVIEW_MANIFEST_ARTIFACT, "workspace": fixture.binding.owner_destination}),
        )
        .unwrap();
    assert_eq!(
        read["content"].as_str().unwrap().as_bytes(),
        pinned.as_slice()
    );

    let (source, report) = fixture.report("report.json", &fixture.attempt_id);
    let put = json!({"id": TASK, "path": REVIEW_REPORT_ARTIFACT, "source_path": source, "model": "codex"});
    fixture.forwarded(&broker, PUT, put.clone()).unwrap();
    // A lost answer after the owner committed: the retry carries the same
    // bytes, which the owner's attach treats as a replay.
    fixture.owner.lose_next_put.store(true, Ordering::SeqCst);
    let lost = fixture.forwarded(&broker, PUT, put.clone()).unwrap_err();
    assert!(lost.contains("ended before answering"), "{lost}");
    fixture.forwarded(&broker, PUT, put).unwrap();

    let calls = fixture.owner.calls.lock().unwrap().clone();
    for (name, input, session) in &calls {
        assert_eq!(
            session.worker_invocation.as_ref(),
            Some(&fixture.binding),
            "{name} rides the claim's binding"
        );
        assert_eq!(input["workspace"], fixture.binding.owner_destination);
        assert!(
            input.get("source_path").is_none(),
            "no path leaves the sandbox: {input}"
        );
    }
    let puts = fixture.owner.puts();
    assert_eq!(puts.len(), 3);
    for put in &puts {
        assert_eq!(put["id"], TASK);
        assert_eq!(put["artifacts"][0]["path"], REVIEW_REPORT_ARTIFACT);
        let content: Vec<u8> =
            serde_json::from_value(put["artifacts"][0]["content"].clone()).unwrap();
        assert_eq!(
            content, report,
            "the owner receives the report's exact bytes"
        );
    }

    let rows = fixture
        .runtime
        .list_audit_events(None, Some(PUT.to_string()), None, None, 10)
        .unwrap();
    assert_eq!(
        rows.len(),
        3,
        "one row per bridged call, written by the broker: {rows:?}"
    );
    assert!(rows.iter().all(|row| row.brokered
        && row.peer_pid == Some(std::process::id())
        && row.task_id.as_deref() == Some(TASK)
        && row.activity_id.as_deref() == Some("agent_review_repair")));
}

#[test]
fn the_broker_refuses_what_its_records_do_not_admit_without_reaching_the_owner() {
    let fixture = Fixture::new();
    let broker = fixture.serve("agent_review_repair");
    let (_, report) = fixture.report("report.json", &fixture.attempt_id);
    let (stale_source, _) = fixture.report("stale.json", "attempt-of-another-run");
    let encoded = {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode(&report)
    };

    let cases = [
        (
            "the gate's record",
            GET,
            json!({"id": TASK, "path": "review-gate.json"}),
        ),
        (
            "a path outside the artifacts",
            GET,
            json!({"id": TASK, "path": "../review-report.json"}),
        ),
        (
            "a write of another path",
            PUT,
            json!({"id": TASK, "path": REVIEW_EVIDENCE_HOLD_ARTIFACT, "source_path": stale_source}),
        ),
        (
            "another task",
            GET,
            json!({"id": "TSO-2", "path": REVIEW_MANIFEST_ARTIFACT}),
        ),
        (
            "another attempt's report",
            PUT,
            json!({"id": TASK, "path": REVIEW_REPORT_ARTIFACT, "source_path": stale_source}),
        ),
    ];
    for (case, name, input) in cases {
        let refused = fixture.forwarded(&broker, name, input).unwrap_err();
        // The claim's scope refuses another task and a path outside the
        // artifacts; the reviewer's attempt scope refuses the rest.
        assert!(refused.contains("_bridge_refused"), "{case}: {refused}");
    }
    // What no nested `orbit` sends: fields that would widen the scope, a
    // certificate, and a tool outside the two.
    let serving = super::super::brokered::RunDispatch::new(
        fixture.runtime.clone(),
        fixture.run("agent_review_repair"),
    );
    for (case, tool, input) in [
        (
            "claim override",
            PUT,
            json!({"id": TASK, "path": REVIEW_REPORT_ARTIFACT, "content_base64": encoded, "claim_id": "forged"}),
        ),
        (
            "workspace override",
            GET,
            json!({"id": TASK, "path": REVIEW_MANIFEST_ARTIFACT, "workspace": "elsewhere/ws_x"}),
        ),
        (
            "certificate",
            PUT,
            json!({"id": TASK, "path": "review-certificate.json", "content_base64": encoded}),
        ),
        (
            "undecodable",
            PUT,
            json!({"id": TASK, "path": REVIEW_REPORT_ARTIFACT, "content_base64": "%%"}),
        ),
        (
            "another tool",
            "orbit.task.update",
            json!({"id": TASK, "status": "done"}),
        ),
    ] {
        let refused = crate::runtime::plugin::broker::BrokerDispatch::call(
            &serving,
            crate::runtime::plugin::broker::BrokerRequest {
                tool: tool.into(),
                input,
                cwd: fixture.worktree.clone(),
                workspace: None,
                entry_point: crate::runtime::plugin::broker::EntryPoint::Cli,
                dry_run: false,
            },
            std::process::id(),
            Arc::new(AtomicBool::new(false)),
        );
        assert!(refused.is_err(), "{case}: {refused:?}");
    }
    assert!(
        fixture.owner.calls.lock().unwrap().is_empty(),
        "nothing refused reaches the owner"
    );

    // The owner's manifest for another attempt is stale.
    fixture.pin_manifest("attempt-of-another-run");
    let stale = fixture
        .forwarded(
            &broker,
            GET,
            json!({"id": TASK, "path": REVIEW_MANIFEST_ARTIFACT}),
        )
        .unwrap_err();
    assert!(stale.contains("review_manifest_stale"), "{stale}");

    // Another activity's broker carries nothing; nor does the reviewer's once
    // its invocation has ended.
    let implementer = fixture.serve("agent_implement");
    let refused = fixture
        .forwarded(
            &implementer,
            GET,
            json!({"id": TASK, "path": REVIEW_MANIFEST_ARTIFACT}),
        )
        .unwrap_err();
    assert!(refused.contains("before-PR reviewer"), "{refused}");
    fixture.pin_manifest(&fixture.attempt_id.clone());
    fixture.reviewer(ReviewerInvocationEvent::Finished { runtime_seconds: 5 });
    let ended = fixture
        .forwarded(
            &broker,
            GET,
            json!({"id": TASK, "path": REVIEW_MANIFEST_ARTIFACT}),
        )
        .unwrap_err();
    assert!(ended.contains("review_attempt_stale"), "{ended}");
    assert!(fixture.owner.puts().is_empty());

    let rows = fixture
        .runtime
        .list_audit_events(None, None, None, None, 50)
        .unwrap();
    assert!(
        rows.iter()
            .filter(|row| row.brokered)
            .all(|row| row.status != AuditEventStatus::Success),
        "every refusal is audited as one: {rows:?}"
    );
}

/// The prior evidence a continuing review reads: the contract's own report,
/// history and hold, and exactly the evidence in the gate's `review-*`
/// namespace the owner's hold names. The names come from the owner's hold; a
/// path no hold names is refused. Evidence outside that namespace is an
/// ordinary artifact of the claimed task (the claimed-owner tests).
#[test]
fn the_reviewer_reads_the_prior_evidence_the_owners_hold_names() {
    let fixture = Fixture::new();
    let broker = fixture.serve("agent_review_repair");
    let read = |path: &str| {
        fixture
            .forwarded(&broker, GET, json!({"id": TASK, "path": path}))
            .map(|output| output["content"].as_str().unwrap_or_default().to_string())
    };
    let named = |path: &str| {
        let refused = read(path).unwrap_err();
        assert!(
            refused.contains("claimed_review_bridge_refused"),
            "{path}: {refused}"
        );
    };
    let candidate = json!({"commit": "a".repeat(40), "tree": "b".repeat(40)});
    let (_, report) = fixture.report("report.json", &fixture.attempt_id);
    fixture.owner.hold(REVIEW_REPORT_ARTIFACT, &report);
    fixture
        .owner
        .hold(REVIEW_REPORT_HISTORY_ARTIFACT, br#"{"schema_version":1}"#);
    let result = json!({
        "schema_version": 1, "attempt_id": "attempt-held", "candidate": candidate,
        "kind": "hosted_ci", "name": "CI", "command": "make ci",
        "outcome": "passed", "log_artifact": "review-evidence-ci.log",
    })
    .to_string();
    fixture
        .owner
        .hold("review-evidence-ci.json", result.as_bytes());
    fixture.owner.hold("review-evidence-ci.log", b"ci passed");
    fixture.owner.hold("review-evidence-unnamed.json", b"{}");

    // The contract's own artifacts are carried; with no hold, nothing names
    // the evidence, and an absent artifact is the owner's not-found.
    assert_eq!(
        read(REVIEW_REPORT_ARTIFACT).unwrap().as_bytes(),
        report.as_slice()
    );
    assert!(read(REVIEW_REPORT_HISTORY_ARTIFACT).is_ok());
    let absent = read(REVIEW_EVIDENCE_HOLD_ARTIFACT).unwrap_err();
    assert!(absent.contains("not found"), "{absent}");
    named("review-evidence-ci.json");

    let hold = json!({
        "schema_version": 1, "attempt_id": "attempt-held", "lineage_key": LINEAGE,
        "run_id": LEAF, "candidate": candidate, "task_meaning_digest": "digest",
        "requirements": [{"kind": "hosted_ci", "name": "CI", "command": "make ci",
                          "artifact": "review-evidence-ci.json"}],
    })
    .to_string();
    fixture
        .owner
        .hold(REVIEW_EVIDENCE_HOLD_ARTIFACT, hold.as_bytes());
    assert_eq!(read(REVIEW_EVIDENCE_HOLD_ARTIFACT).unwrap(), hold);
    assert_eq!(read("review-evidence-ci.json").unwrap(), result);
    assert_eq!(read("review-evidence-ci.log").unwrap(), "ci passed");
    named("review-evidence-unnamed.json");

    // A result for another candidate does not name its log.
    let foreign = result.replace(&"a".repeat(40), &"e".repeat(40));
    fixture
        .owner
        .hold("review-evidence-ci.json", foreign.as_bytes());
    named("review-evidence-ci.log");

    let reads = fixture.owner.reads();
    assert!(
        !reads
            .iter()
            .any(|path| path == "review-evidence-unnamed.json"),
        "an unnamed path never reaches the owner: {reads:?}"
    );
    assert!(fixture.owner.puts().is_empty());
}

/// A reviewer whose recorded deadline has passed is no longer running, even
/// though no `Finished` event was recorded for it: a hung or killed reviewer
/// leaves exactly that ledger behind.
#[test]
fn a_reviewer_past_its_deadline_without_finishing_reaches_nothing() {
    let fixture = Fixture::new();
    let broker = fixture.serve("agent_review_repair");
    fixture.reviewer_at(
        ReviewerInvocationEvent::Started {
            timeout_seconds: 1800,
        },
        Utc::now() - chrono::Duration::hours(1),
    );
    let (source, _) = fixture.report("report.json", &fixture.attempt_id);
    for (name, input) in [
        (GET, json!({"id": TASK, "path": REVIEW_MANIFEST_ARTIFACT})),
        (
            PUT,
            json!({"id": TASK, "path": REVIEW_REPORT_ARTIFACT, "source_path": source}),
        ),
    ] {
        let refused = fixture.forwarded(&broker, name, input).unwrap_err();
        assert!(
            refused.contains("review_attempt_stale"),
            "{name}: {refused}"
        );
    }
    assert!(
        fixture.owner.calls.lock().unwrap().is_empty(),
        "an expired reviewer's calls never reach the owner"
    );
}
