//! A claimed follower's notice calls through the broker to a real owner
//! store: stored only there, attributed to the claim's task and run, and
//! refused without reaching the owner when the claim does not match.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use orbit_common::OrbitError;
use orbit_engine::PluginBrokerHandle as _;
use orbit_tools::OwnerCoordinator;
use orbit_types::tool::{ToolSessionContext, WorkerInvocation};
use serde_json::{Value, json};

use super::super::super::owner::bridge_through;
use super::super::review::{FOLLOWER, Fixture, OWNER};
use super::broker::refused;
use crate::OrbitRuntime;
use crate::adapter::command::dispatch::brokered::RunDispatch;
use crate::adapter::command::dispatch::execute::ToolEntryPoint;
use crate::runtime::plugin::broker::{BrokerDispatch as _, BrokerRequest, EntryPoint};

const POST: &str = "orbit.notice.post";
const SHOW: &str = "orbit.notice.show";
const CLOSE: &str = "orbit.notice.close";

/// The owner side: the owner's public tools on a real store, carrying the
/// broker session (and its claim binding) into the owner's dispatch.
struct NoticeOwner {
    runtime: OrbitRuntime,
    calls: AtomicUsize,
    received: Mutex<Vec<(String, Value, ToolSessionContext)>>,
}

impl NoticeOwner {
    fn execute(
        &self,
        tool: &str,
        mut input: Value,
        session: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        input["workspace"] = json!(self.runtime.workspace_id()?);
        self.runtime
            .execute_tool_command_with_session_context(tool, input, None, None, session)
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl OwnerCoordinator for NoticeOwner {
    fn call(
        &self,
        name: &str,
        input: Value,
        session: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        let binding = session.worker_invocation.as_ref().expect("binding");
        assert_eq!(input["workspace"], binding.owner_destination);
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.received
            .lock()
            .unwrap()
            .push((name.to_string(), input.clone(), session.clone()));
        self.execute(name, input, session)
    }
}

struct NoticeFixture {
    fixture: Fixture,
    owner: Arc<NoticeOwner>,
    claimed: String,
    related: String,
}

impl NoticeFixture {
    fn new() -> Self {
        let mut fixture = Fixture::new();
        let owner_runtime =
            OrbitRuntime::from_roots(&fixture.global_root, &fixture.worktree.join(".orbit"))
                .unwrap()
                .without_worker_routing();
        let owner = Arc::new(NoticeOwner {
            runtime: owner_runtime,
            calls: AtomicUsize::new(0),
            received: Mutex::new(Vec::new()),
        });
        let mut ids = Vec::new();
        for title in ["Claimed task", "Related task"] {
            let task = owner
                .execute(
                    "orbit.task.add",
                    json!({"title": title, "description": "Notice broker regression",
                           "complexity": "low"}),
                    ToolSessionContext::default(),
                )
                .unwrap();
            ids.push(task["id"].as_str().unwrap().to_string());
        }
        let workspace_id = owner.runtime.workspace_id().unwrap();
        fixture.binding.task_id = ids[0].clone();
        fixture.binding.owner_workspace_id = workspace_id.clone();
        fixture.binding.owner_destination = format!("{OWNER}/{workspace_id}");
        fixture.runtime = fixture
            .runtime
            .without_worker_routing()
            .with_worker_invocation(fixture.binding.clone(), owner.clone())
            .unwrap();
        Self {
            fixture,
            owner,
            claimed: ids[0].clone(),
            related: ids[1].clone(),
        }
    }

    fn serve(&self) -> crate::runtime::plugin::broker::PluginBroker {
        let mut run = self.fixture.run("agent_implement");
        run.task_id = Some(self.fixture.binding.task_id.clone());
        let dispatch = Arc::new(RunDispatch::new(self.fixture.runtime.clone(), run.clone()));
        let broker = crate::runtime::plugin::broker::PluginBroker::start(
            &self.fixture.global_root,
            &run.run_id,
            dispatch,
        )
        .unwrap();
        broker.bind_anchor(crate::runtime::plugin::broker::PeerAnchor::Ancestor(
            orbit_common::process::ancestry::process_start_key(std::process::id()).unwrap(),
        ));
        broker
    }

    fn forwarded(
        &self,
        broker: &crate::runtime::plugin::broker::PluginBroker,
        name: &str,
        input: Value,
    ) -> Result<Value, String> {
        self.fixture.forwarded(broker, name, input)
    }
}

#[test]
fn claimed_follower_posts_shows_and_closes_its_notice_on_the_owner_through_the_broker() {
    if crate::application::run_isolated_test(std::any::type_name_of_val(
        &claimed_follower_posts_shows_and_closes_its_notice_on_the_owner_through_the_broker,
    )) {
        return;
    }
    let notice = NoticeFixture::new();
    let broker = notice.serve();
    let binding = notice.fixture.binding.clone();

    // A linked task on the same owner (not only the claimed task) is admitted.
    let posted = notice
        .forwarded(
            &broker,
            POST,
            json!({"kind": "info", "title": "Broker notice probe", "linked_task": notice.related,
                   "orchestrator": "not-a-crew", "model": "codex"}),
        )
        .unwrap();
    let stored = &posted["notice"];
    let id = stored["id"].as_str().expect("notice id").to_string();
    assert_eq!(stored["author"]["class"], "agent", "{posted}");
    assert_eq!(stored["author"]["task_id"], notice.claimed, "{posted}");
    assert_eq!(stored["author"]["run_id"], binding.bound_run_id, "{posted}");
    assert_eq!(stored["linked_task"]["id"], notice.related, "{posted}");
    assert_eq!(
        stored["workspace"]["id"],
        notice.owner.runtime.workspace_id().unwrap(),
        "{posted}"
    );

    let shown = notice.forwarded(&broker, SHOW, json!({"id": id})).unwrap();
    assert_eq!(shown["notice"]["id"], id, "{shown}");
    assert_eq!(shown["notice"]["author"]["task_id"], notice.claimed);

    let closed = notice
        .forwarded(
            &broker,
            CLOSE,
            json!({"id": id, "reason": "probe finished"}),
        )
        .unwrap();
    assert_eq!(closed["notice"]["status"], "closed", "{closed}");

    // Every brokered call rode the claim binding, in the bound workspace,
    // and no caller-named orchestrator reached the owner.
    let received = notice.owner.received.lock().unwrap().clone();
    assert_eq!(
        received
            .iter()
            .map(|(name, ..)| name.as_str())
            .collect::<Vec<_>>(),
        vec![POST, SHOW, CLOSE]
    );
    for (name, input, session) in &received {
        assert_eq!(session.worker_invocation.as_ref(), Some(&binding), "{name}");
        assert!(input.get("orchestrator").is_none(), "{name}: {input}");
    }

    // Poster restrictions stay the owner's: an agent may not post a
    // global or maintenance notice, and an internal field never crosses.
    for input in [
        json!({"kind": "info", "title": "Global probe", "scope": "global", "linked_task": notice.claimed}),
        json!({"kind": "maintenance", "title": "Maintenance probe", "linked_task": notice.claimed}),
    ] {
        assert!(notice.forwarded(&broker, POST, input).is_err());
    }
    let before = notice.owner.calls();
    refused(notice.fixture.call(
        Some(broker.socket_path()),
        POST,
        json!({"kind": "info", "title": "Internal probe", "linked_task": notice.claimed, "_worker_read": "x"}),
    ));
    assert_eq!(
        notice.owner.calls(),
        before,
        "an internal field never crosses the broker"
    );

    // A run whose task is not the claimed task carries no notice call.
    let before = notice.owner.calls();
    let mut other = notice.fixture.run("agent_implement");
    other.task_id = Some("TSO-999".into());
    let mismatched = RunDispatch::new(notice.fixture.runtime.clone(), other).call(
        BrokerRequest {
            tool: POST.into(),
            input: json!({"kind": "info", "title": "Stale claim", "linked_task": notice.claimed}),
            cwd: notice.fixture.worktree.clone(),
            workspace: None,
            entry_point: EntryPoint::Cli,
            dry_run: false,
        },
        std::process::id(),
        Arc::new(std::sync::atomic::AtomicBool::new(false)),
    );
    assert!(
        mismatched
            .unwrap_err()
            .to_string()
            .contains("the run's task is not the claimed task")
    );
    assert_eq!(
        notice.owner.calls(),
        before,
        "a mismatched claim never reaches the owner"
    );

    // On the owner's own machine a notice keeps the in-process route, and a
    // workspace other than the binding's is refused before it.
    let local = WorkerInvocation {
        owner_machine_id: FOLLOWER.into(),
        ..binding.clone()
    };
    let bridge = |name: &str, input: &Value| {
        bridge_through(
            None,
            &notice.fixture.global_root,
            Some(&local),
            Some(FOLLOWER),
            name,
            input,
            &notice.fixture.worktree,
            &notice.fixture.worktree,
            ToolEntryPoint::Cli,
        )
    };
    assert!(bridge(POST, &json!({"kind": "info", "title": "Local"})).is_none());
    let error = refused(bridge(
        POST,
        &json!({"kind": "info", "title": "Local", "workspace": "hm_owner/ws_other"}),
    ));
    assert!(
        error
            .to_string()
            .contains("worker workspace binding mismatch"),
        "{error}"
    );
}
