//! Regression for a claimed worker refused another task's record and
//! artifacts in its claimed workspace (ORB-19270, office F2026-10-068): a
//! design-sync run on a follower could list related tasks but not open them.
//! Exercise the bridge, the broker's real socket and the owner's public task
//! tools on a real store, for a remote owner and for an owner on the
//! worker's own machine. The local route once skipped the claim's
//! `review-*` and delivery-run refusals the broker applies (ORB-19289).
//! Same-task review reads must also reach the broker's running-reviewer
//! check: the implementer shares the reviewer's worker binding (ORB-19359).

use std::sync::Arc;

use orbit_common::OrbitError;
use orbit_engine::PluginBrokerHandle;
use orbit_types::tool::{ToolSessionContext, WorkerInvocation};
use orbit_types::workflow::{
    REVIEW_EVIDENCE_HOLD_ARTIFACT, REVIEW_MANIFEST_ARTIFACT, REVIEW_REPORT_ARTIFACT,
    REVIEW_REPORT_HISTORY_ARTIFACT,
};
use serde_json::{Value, json};

use super::super::super::owner::{ClaimedOwnerRoute, bridge_through};
use super::super::review::{FOLLOWER, Fixture, GET, PUT};
use super::broker::{SHOW, TaskOwner, refused};
use crate::adapter::command::dispatch::execute::ToolEntryPoint;

/// The claimed task and a related task in the same workspace, each with a
/// comment and an artifact, on the owner's store.
struct Workspace {
    fixture: Fixture,
    owner: Arc<TaskOwner>,
    related: String,
}

impl Workspace {
    fn new() -> Self {
        let mut fixture = Fixture::new();
        let owner = Arc::new(TaskOwner::over(&fixture.runtime));
        let mut ids = Vec::new();
        for title in ["Claimed task", "Related task"] {
            let task = owner
                .execute(
                    "orbit.task.add",
                    json!({"title": title, "description": "Workspace read regression",
                           "complexity": "low"}),
                )
                .unwrap();
            let id = task["id"].as_str().unwrap().to_string();
            for index in 0..2 {
                owner
                    .execute(
                        "orbit.task.update",
                        json!({"id": id, "comment": format!("comment {index}")}),
                    )
                    .unwrap();
            }
            let source = fixture.worktree.join(".orbit/tmp/ledger.json");
            std::fs::write(&source, format!(r#"{{"task":"{id}"}}"#)).unwrap();
            owner
                .execute(
                    PUT,
                    json!({"id": id, "path": "ledger.json", "source_path": source}),
                )
                .unwrap();
            ids.push(id);
        }
        fixture.binding.task_id = ids[0].clone();
        Self {
            fixture,
            owner,
            related: ids[1].clone(),
        }
    }

    /// The follower's runtime bound to `binding`, routing to this owner.
    fn bind(&mut self, binding: WorkerInvocation) {
        self.fixture.binding = binding.clone();
        self.fixture.runtime = self
            .fixture
            .runtime
            .without_worker_routing()
            .with_worker_invocation(binding, self.owner.clone())
            .unwrap();
    }

    /// The owner's own answer to `input`, as the bridge must return it.
    fn direct(&self, name: &str, input: &Value) -> Result<Value, String> {
        let mut input = input.clone();
        if input.get("limit").is_some() {
            input["view"] = json!("bounded");
        }
        self.owner
            .execute(name, input)
            .map_err(|error| error.to_string())
    }

    /// Every read of the related task the claim now carries.
    fn reads(&self) -> Vec<(&'static str, Value)> {
        let id = &self.related;
        vec![
            (SHOW, json!({"id": id})),
            (SHOW, json!({"id": id, "fields": ["status", "comments"]})),
            (SHOW, json!({"id": id, "limit": 1, "comments_offset": 1})),
            (SHOW, json!({"id": id, "field": "delivery"})),
            (GET, json!({"id": id, "path": "ledger.json"})),
        ]
    }

    /// Requests the claim still refuses, whatever task they name.
    fn refusals(&self) -> Vec<(&'static str, Value)> {
        let id = &self.related;
        let source = self.fixture.worktree.join(".orbit/tmp/ledger.json");
        vec![
            (SHOW, json!({"id": id, "workspace": "hm_owner/ws_other"})),
            (SHOW, json!({"id": id, "view": "bounded"})),
            (SHOW, json!({"id": id, "snapshot": true})),
            (SHOW, json!({"id": id, "_worker_read": "tasks"})),
            (
                GET,
                json!({"id": id, "path": "ledger.json", "_worker_read": "artifact"}),
            ),
            (GET, json!({"id": id, "path": "../ledger.json"})),
            // Another task's review artifacts stay outside the claim.
            (GET, json!({"id": id, "path": REVIEW_MANIFEST_ARTIFACT})),
            (GET, json!({"id": id, "path": REVIEW_REPORT_ARTIFACT})),
            (
                PUT,
                json!({"id": id, "path": "evidence.json", "source_path": source}),
            ),
        ]
    }
}

/// The answer matches the owner's own, whether the owner answers or refuses.
/// A bounded view stamps when it was observed, which two reads cannot share.
fn same_answer(case: &str, bridged: Result<Value, String>, direct: Result<Value, String>) {
    let unstamped = |mut value: Value| {
        if let Some(object) = value.as_object_mut() {
            object.remove("observed_at");
        }
        value
    };
    match direct {
        Ok(direct) => assert_eq!(bridged.map(unstamped), Ok(unstamped(direct)), "{case}"),
        Err(direct) => {
            let bridged = bridged.expect_err(case);
            assert!(bridged.contains(&direct), "{case}: {bridged} vs {direct}");
        }
    }
}

fn outside_workspace(error: &str) -> bool {
    error.contains("claimed_owner_bridge_refused")
        && error.contains("is not a task in the claimed task's workspace")
}

fn scope_refusal(error: &str) -> bool {
    [
        "claimed_owner_bridge_refused",
        "does not accept",
        "workspace binding mismatch",
    ]
    .iter()
    .any(|reason| error.contains(reason))
}

#[test]
fn a_remote_owners_claimed_worker_reads_any_task_of_its_claimed_workspace() {
    if crate::application::run_isolated_test(std::any::type_name_of_val(
        &a_remote_owners_claimed_worker_reads_any_task_of_its_claimed_workspace,
    )) {
        return;
    }
    let mut workspace = Workspace::new();
    workspace.bind(workspace.fixture.binding.clone());
    let fixture = &workspace.fixture;
    let mut run = fixture.run("agent_implement");
    run.task_id = Some(fixture.binding.task_id.clone());
    let dispatch = Arc::new(
        crate::adapter::command::dispatch::brokered::RunDispatch::new(
            fixture.runtime.clone(),
            run.clone(),
        ),
    );
    let broker = crate::runtime::plugin::broker::PluginBroker::start(
        &fixture.global_root,
        &run.run_id,
        dispatch,
    )
    .unwrap();
    broker.bind_anchor(crate::runtime::plugin::broker::PeerAnchor::Ancestor(
        orbit_common::process::ancestry::process_start_key(std::process::id()).unwrap(),
    ));

    for (name, input) in workspace.reads() {
        let before = workspace.owner.calls();
        let bridged = fixture.forwarded(&broker, name, input.clone());
        assert!(
            workspace.owner.calls() > before,
            "{name} {input} reaches the owner"
        );
        same_answer(
            &format!("{name} {input}"),
            bridged,
            workspace.direct(name, &input),
        );
    }
    let page = fixture
        .forwarded(
            &broker,
            SHOW,
            json!({"id": workspace.related, "limit": 1, "comments_offset": 1}),
        )
        .unwrap();
    assert_eq!(page["task"]["id"], workspace.related);
    assert_eq!(page["comments_pagination"]["offset"], 1);
    let ledger = fixture
        .forwarded(
            &broker,
            GET,
            json!({"id": workspace.related, "path": "ledger.json"}),
        )
        .unwrap();
    assert_eq!(
        ledger["content"],
        format!(r#"{{"task":"{}"}}"#, workspace.related)
    );

    // A task the owner does not hold in the claimed workspace is outside the
    // claim, for a record and an artifact alike; a missing artifact of a task
    // it does hold stays the owner's not-found.
    for (name, input) in [
        (SHOW, json!({"id": "TSX-404"})),
        (SHOW, json!({"id": "TSX-404", "field": "delivery"})),
        (GET, json!({"id": "TSX-404", "path": "ledger.json"})),
    ] {
        let error = fixture.forwarded(&broker, name, input.clone()).unwrap_err();
        assert!(outside_workspace(&error), "{name} {input}: {error}");
    }
    let error = fixture
        .forwarded(
            &broker,
            GET,
            json!({"id": workspace.related, "path": "missing.json"}),
        )
        .unwrap_err();
    assert!(
        !outside_workspace(&error) && error.contains("not found"),
        "{error}"
    );

    let before = workspace.owner.calls();
    for (name, input) in workspace.refusals() {
        let error = match fixture.call(Some(broker.socket_path()), name, input.clone()) {
            Some(ClaimedOwnerRoute::Forwarded(Err(error)))
            | Some(ClaimedOwnerRoute::Refused(error)) => error.to_string(),
            other => panic!("{name} {input} is refused: {other:?}"),
        };
        assert!(scope_refusal(&error), "{name} {input}: {error}");
    }
    assert_eq!(
        workspace.owner.calls(),
        before,
        "scope refusals never reach the owner"
    );
}

#[test]
fn a_claimed_worker_on_its_owners_machine_gets_the_same_workspace_reads_and_refusals() {
    if crate::application::run_isolated_test(std::any::type_name_of_val(
        &a_claimed_worker_on_its_owners_machine_gets_the_same_workspace_reads_and_refusals,
    )) {
        return;
    }
    let mut workspace = Workspace::new();
    let local = WorkerInvocation {
        owner_machine_id: FOLLOWER.into(),
        ..workspace.fixture.binding.clone()
    };
    workspace.bind(local.clone());
    let fixture = &workspace.fixture;
    let bridge = |name: &str, input: &Value| {
        bridge_through(
            None,
            &fixture.global_root,
            Some(&local),
            Some(FOLLOWER),
            name,
            input,
            &fixture.worktree,
            &fixture.worktree,
            ToolEntryPoint::Cli,
        )
    };
    // The local owner route, which a call the bridge leaves alone takes.
    let route = |name: &str, input: Value| {
        fixture
            .runtime
            .route_worker_tool(name, input, ToolSessionContext::default())
            .map_err(|error| error.to_string())
    };

    for (name, input) in workspace.reads() {
        assert!(
            bridge(name, &input).is_none(),
            "{name} {input} keeps the local owner route"
        );
        let before = workspace.owner.calls();
        let answer = route(name, input.clone());
        assert!(workspace.owner.calls() > before, "{name} {input}");
        // The local route passes the request as it is, without selecting
        // the bounded view.
        let direct = workspace
            .owner
            .execute(name, input.clone())
            .map_err(|error| error.to_string());
        same_answer(&format!("{name} {input}"), answer, direct);
    }
    for (name, input) in [
        (SHOW, json!({"id": "TSX-404"})),
        (GET, json!({"id": "TSX-404", "path": "ledger.json"})),
    ] {
        assert!(bridge(name, &input).is_none(), "{name} {input}");
        let error = route(name, input.clone()).unwrap_err();
        assert!(outside_workspace(&error), "{name} {input}: {error}");
    }

    let before = workspace.owner.calls();
    for (name, input) in workspace.refusals() {
        let error = refused(bridge(name, &input));
        assert!(
            matches!(
                &error,
                OrbitError::PolicyDenied(_) | OrbitError::InvalidInput(_)
            ) && scope_refusal(&error.to_string()),
            "{name} {input}: {error}"
        );
    }
    // The claimed task's delivery is its leaf run's only.
    let error = refused(bridge(
        SHOW,
        &json!({"id": local.task_id, "field": "delivery", "run_id": "jrun-other"}),
    ));
    assert!(
        error.to_string().contains("delivery_run_refused"),
        "another run's delivery: {error}"
    );
    assert_eq!(workspace.owner.calls(), before);
    for input in [
        json!({"id": local.task_id, "field": "delivery"}),
        json!({"id": local.task_id, "field": "delivery", "run_id": local.bound_run_id}),
    ] {
        assert!(bridge(SHOW, &input).is_none(), "{input}");
    }

    // The claimed task stays readable, and its own artifact writable.
    assert!(bridge(SHOW, &json!({"id": local.task_id})).is_none());
    let source = fixture.worktree.join(".orbit/tmp/ledger.json");
    assert!(
        bridge(
            PUT,
            &json!({"id": local.task_id, "path": "evidence.json", "source_path": source}),
        )
        .is_none()
    );
}

#[test]
fn a_claimed_worker_on_its_owners_machine_reads_and_writes_review_artifacts_only_as_the_running_reviewer()
 {
    let mut fixture = Fixture::new();
    let local = WorkerInvocation {
        owner_machine_id: FOLLOWER.into(),
        ..fixture.binding.clone()
    };
    fixture.runtime = fixture
        .runtime
        .without_worker_routing()
        .with_worker_invocation(local.clone(), fixture.owner.clone())
        .unwrap();
    fixture.binding = local;
    let (report, bytes) = fixture.report("report.json", &fixture.attempt_id);
    fixture.owner.hold(REVIEW_REPORT_ARTIFACT, &bytes);
    fixture
        .owner
        .hold(REVIEW_REPORT_HISTORY_ARTIFACT, br#"{"revisions":[]}"#);
    fixture
        .owner
        .hold(REVIEW_EVIDENCE_HOLD_ARTIFACT, br#"{"requirements":[]}"#);
    let contracts = [
        REVIEW_MANIFEST_ARTIFACT,
        REVIEW_REPORT_ARTIFACT,
        REVIEW_REPORT_HISTORY_ARTIFACT,
        REVIEW_EVIDENCE_HOLD_ARTIFACT,
    ];
    let put = json!({"id": fixture.binding.task_id, "path": REVIEW_REPORT_ARTIFACT,
                     "source_path": report});

    // The implementer shares the reviewer's leaf binding, so only the run's
    // broker can tell them apart; a report naming the open attempt is still
    // refused for it.
    let implementer = fixture.serve("agent_implement");
    // Same-task reads once bypassed the broker on a local owner. Even the
    // contract and respelled review names must reach its reviewer check.
    for path in contracts.into_iter().chain([
        "review-gate.json",
        "review-baseline.json",
        "review-evidence-private.log",
        " ./review-report.json ",
        "REVIEW-REPORT.JSON",
    ]) {
        let error = fixture
            .forwarded(
                &implementer,
                GET,
                json!({"id": fixture.binding.task_id, "path": path}),
            )
            .unwrap_err();
        assert!(
            error.contains("claimed_review_bridge_refused") && error.contains("before-PR reviewer"),
            "{path}: {error}"
        );
    }
    assert!(
        fixture.owner.reads().is_empty(),
        "a non-reviewer's same-task review reads never reach the owner"
    );
    let error = fixture
        .forwarded(&implementer, PUT, put.clone())
        .unwrap_err();
    assert!(
        error.contains("claimed_review_bridge_refused") && error.contains("before-PR reviewer"),
        "{error}"
    );
    assert!(
        fixture.owner.puts().is_empty(),
        "the forged report never lands"
    );

    let reviewer = fixture.serve("agent_review_repair");
    for path in contracts {
        let read = fixture
            .forwarded(
                &reviewer,
                GET,
                json!({"id": fixture.binding.task_id, "path": path}),
            )
            .unwrap();
        assert_eq!(read["path"], path);
        assert_eq!(read["id"], fixture.binding.task_id);
        assert!(
            read["content"]
                .as_str()
                .is_some_and(|text| !text.is_empty())
        );
    }
    assert_eq!(fixture.owner.reads(), contracts);
    fixture.forwarded(&reviewer, PUT, put).unwrap();
    let puts = fixture.owner.puts();
    assert_eq!(puts.len(), 1);
    let content: Vec<u8> =
        serde_json::from_value(puts[0]["artifacts"][0]["content"].clone()).unwrap();
    assert_eq!(content, bytes, "the running reviewer's report lands");
}
