//! A claimed worker's owner calls between the nested `orbit`'s bridge and
//! [`RunDispatch`] over the broker's real socket, against an owner route that
//! records what reaches it (the claimed-review fixture).
//!
//! Admitted as a security invariant the boundary cannot reach on a host
//! without a confined Mac leaf: the broker's claim scope comes only from the
//! run's records, a refused or cross-task request never reaches the owner,
//! and inside a masked sandbox no call is attempted over SSH — a missing or
//! gone coordinator is the typed `owner_route_unavailable` instead.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use orbit_common::OrbitError;
use orbit_engine::PluginBrokerHandle;
use orbit_types::tool::WorkerInvocation;
use orbit_types::workflow::{OWNER_ROUTE_UNAVAILABLE_ERROR_CODE, REVIEW_REPORT_ARTIFACT};
use serde_json::{Value, json};

use super::super::brokered::RunDispatch;
use super::super::claimed_owner::{ClaimedOwnerRoute, bridge_through};
use super::super::execute::ToolEntryPoint;
use super::claimed_review::{FOLLOWER, Fixture, GET, PUT, TASK};
use crate::runtime::plugin::broker::{BrokerDispatch, BrokerRequest, EntryPoint};

const SHOW: &str = "orbit.task.show";
const ADD: &str = "orbit.task.add";
const FRICTION: &str = "orbit.friction.add";

fn spawned_from(target: &str) -> Value {
    json!([{"type": "spawned_from", "target": target}])
}

/// Stand in for the agent sandbox's mask: the Linux sentinel over the plugin
/// secret store.
fn mask(fixture: &Fixture) {
    let sentinel = crate::runtime::plugin::paths::plugin_secret_store_dir(&fixture.global_root)
        .join(crate::runtime::plugin::sandbox_mask::PLUGIN_MASK_SENTINEL_FILE);
    std::fs::create_dir_all(sentinel.parent().unwrap()).unwrap();
    std::fs::write(&sentinel, b"masked").unwrap();
}

fn refused(route: Option<ClaimedOwnerRoute>) -> OrbitError {
    match route {
        Some(ClaimedOwnerRoute::Refused(error)) => error,
        other => panic!("refused before the broker: {other:?}"),
    }
}

fn route_unavailable(error: &OrbitError) -> bool {
    matches!(error, OrbitError::RemoteTool { code, payload, .. }
        if code == OWNER_ROUTE_UNAVAILABLE_ERROR_CODE
            && payload["code"] == OWNER_ROUTE_UNAVAILABLE_ERROR_CODE)
}

#[test]
fn every_allowlisted_owner_call_reaches_the_owner_through_the_broker_on_the_claim() {
    let fixture = Fixture::new();
    let broker = fixture.serve("agent_implement");
    let evidence = fixture.worktree.join(".orbit/tmp/evidence.json");
    std::fs::write(&evidence, br#"{"checked": true}"#).unwrap();
    let workspace = fixture.binding.owner_destination.clone();

    let calls = [
        (
            SHOW,
            json!({"id": TASK, "fields": ["status"], "workspace": workspace}),
        ),
        (
            ADD,
            json!({"title": "Follow-up", "description": "Found while working.",
                   "complexity": "low", "relations": spawned_from(TASK), "model": "codex"}),
        ),
        (
            FRICTION,
            json!({"body": "The route was slow.", "model": "codex", "during_task": TASK}),
        ),
        (GET, json!({"id": TASK, "path": "notes.md"})),
        (
            PUT,
            json!({"id": TASK, "path": "evidence.json", "source_path": evidence, "model": "codex"}),
        ),
    ];
    for (name, input) in calls.clone() {
        fixture
            .forwarded(&broker, name, input)
            .unwrap_or_else(|error| panic!("{name} reaches the owner: {error}"));
    }

    let received = fixture.owner.calls.lock().unwrap().clone();
    assert_eq!(
        received
            .iter()
            .map(|(name, ..)| name.as_str())
            .collect::<Vec<_>>(),
        calls.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
        "each call reaches the owner once, in order"
    );
    for (name, input, session) in &received {
        assert_eq!(
            session.worker_invocation.as_ref(),
            Some(&fixture.binding),
            "{name} rides the claim's binding"
        );
        assert_eq!(input["workspace"], fixture.binding.owner_destination);
        assert!(input.get("source_path").is_none(), "{name}: {input}");
    }
    assert_eq!(received[1].1["relations"], spawned_from(TASK));
    let put = &received[4].1;
    assert_eq!(put["artifacts"][0]["path"], "evidence.json");
    let content: Vec<u8> = serde_json::from_value(put["artifacts"][0]["content"].clone()).unwrap();
    assert_eq!(content, br#"{"checked": true}"#);

    let rows = fixture
        .runtime
        .list_audit_events(None, None, None, None, 50)
        .unwrap();
    for (name, _) in &calls {
        assert!(
            rows.iter().any(|row| row.brokered
                && row.tool_name.as_deref() == Some(*name)
                && row.task_id.as_deref() == Some(TASK)
                && row.activity_id.as_deref() == Some("agent_implement")),
            "the broker audits {name}: {rows:?}"
        );
    }
}

#[test]
fn the_broker_refuses_cross_task_and_unlisted_calls_without_reaching_the_owner() {
    let fixture = Fixture::new();
    let broker = fixture.serve("agent_implement");
    let (report, _) = fixture.report("report.json", "any-attempt");
    let cases = [
        ("another task's record", SHOW, json!({"id": "TSO-2"})),
        (
            "an internal projection",
            SHOW,
            json!({"id": TASK, "_worker_read": "tasks"}),
        ),
        (
            "a task spawned from nothing",
            ADD,
            json!({"title": "t", "description": "d", "complexity": "low"}),
        ),
        (
            "a task spawned from another task",
            ADD,
            json!({"title": "t", "description": "d", "complexity": "low",
                   "relations": spawned_from("TSO-2")}),
        ),
        (
            "a task that also blocks another",
            ADD,
            json!({"title": "t", "description": "d", "complexity": "low",
                   "relations": [{"type": "spawned_from", "target": TASK},
                                 {"type": "blocks", "target": "TSO-2"}]}),
        ),
        (
            "friction during another task",
            FRICTION,
            json!({"body": "b", "model": "codex", "during_task": "TSO-2"}),
        ),
        (
            "another task's artifact",
            GET,
            json!({"id": "TSO-2", "path": "notes.md"}),
        ),
        (
            "the gate's report outside the reviewer",
            PUT,
            json!({"id": TASK, "path": REVIEW_REPORT_ARTIFACT, "source_path": report}),
        ),
    ];
    for (case, name, input) in cases {
        let error = fixture.forwarded(&broker, name, input).unwrap_err();
        assert!(
            error.contains("bridge_refused") || error.contains("does not accept"),
            "{case}: {error}"
        );
    }

    // What no nested `orbit` sends: a coordination tool outside the list.
    let serving = RunDispatch::new(fixture.runtime.clone(), fixture.run("agent_implement"));
    for (tool, input) in [
        ("orbit.task.update", json!({"id": TASK, "status": "done"})),
        ("orbit.task.list", json!({})),
        (
            "orbit.friction.update",
            json!({"id": "F-1", "status": "resolved"}),
        ),
    ] {
        let refused = serving.call(
            BrokerRequest {
                tool: tool.into(),
                input,
                cwd: fixture.worktree.clone(),
                workspace: None,
                entry_point: EntryPoint::Cli,
                dry_run: false,
            },
            std::process::id(),
            Arc::new(AtomicBool::new(false)),
        );
        assert!(refused.is_err(), "{tool}: {refused:?}");
    }
    assert!(
        fixture.owner.calls.lock().unwrap().is_empty(),
        "nothing refused reaches the owner"
    );

    // A run whose task is not the claimed task carries nothing.
    let mut other = fixture.run("agent_implement");
    other.task_id = Some("TSO-2".into());
    let refused = RunDispatch::new(fixture.runtime.clone(), other).call(
        BrokerRequest {
            tool: SHOW.into(),
            input: json!({"id": TASK}),
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
            .contains("the run's task is not the claimed task")
    );
    assert!(fixture.owner.calls.lock().unwrap().is_empty());
}

#[test]
fn the_nested_orbit_keeps_other_routes_outside_a_masked_sandbox() {
    let fixture = Fixture::new();
    let input = json!({"id": TASK});

    // Not a coordination tool, or no remote owner: the existing route.
    assert!(fixture.call(None, "orbit.search", json!({})).is_none());
    let local_owner = WorkerInvocation {
        owner_machine_id: FOLLOWER.into(),
        ..fixture.binding.clone()
    };
    for binding in [Some(&local_owner), None] {
        assert!(
            bridge_through(
                None,
                &fixture.global_root,
                binding,
                Some(FOLLOWER),
                SHOW,
                &input,
                &fixture.worktree,
                &fixture.worktree,
                ToolEntryPoint::Cli
            )
            .is_none(),
            "an owner on this machine, or an unbound caller, is reached directly"
        );
    }
    // An unsandboxed worker without a broker reaches its owner itself, for
    // any coordination tool.
    assert!(fixture.call(None, SHOW, input).is_none());
    assert!(
        fixture
            .call(None, "orbit.task.update", json!({"id": TASK}))
            .is_none()
    );
}

#[test]
fn inside_a_masked_sandbox_no_owner_call_falls_back_to_ssh() {
    let fixture = Fixture::new();
    mask(&fixture);

    // Without a broker, an allowlisted call is the typed unavailable route.
    for (name, input) in [
        (SHOW, json!({"id": TASK})),
        (FRICTION, json!({"body": "b", "model": "codex"})),
        (GET, json!({"id": TASK, "path": "notes.md"})),
    ] {
        let error = refused(fixture.call(None, name, input));
        assert!(route_unavailable(&error), "{name}: {error}");
        assert!(
            error.to_string().contains("ORBIT_PLUGIN_BROKER is not set"),
            "{error}"
        );
    }
    // A broker that has shut down is the same typed failure.
    let broker = fixture.serve("agent_implement");
    let socket = broker.socket_path().to_path_buf();
    drop(broker);
    let error = refused(fixture.call(Some(&socket), SHOW, json!({"id": TASK})));
    assert!(route_unavailable(&error), "{error}");
    assert!(
        error
            .to_string()
            .contains("could not reach this run's coordinator"),
        "{error}"
    );

    // A coordination tool the broker does not carry is refused, with or
    // without a broker, and is never sent to it.
    let broker = fixture.serve("agent_implement");
    for socket in [None, Some(broker.socket_path())] {
        let error = refused(fixture.call(socket, "orbit.task.update", json!({"id": TASK})));
        assert!(
            matches!(&error, OrbitError::CapabilityDenied(reason)
                if reason.starts_with("claimed_owner_bridge_refused")),
            "{error}"
        );
    }

    // The owner route stood in for SSH: none of it was attempted.
    assert!(fixture.owner.calls.lock().unwrap().is_empty());
}

#[test]
fn a_source_the_nested_orbit_may_not_read_is_refused_before_the_broker() {
    let fixture = Fixture::new();
    // A link out of the workspace, and a file over the artifact limit.
    let outside = fixture.global_root.join("outside.json");
    std::fs::write(&outside, b"{}").unwrap();
    let link = fixture.worktree.join(".orbit/tmp/link.json");
    std::os::unix::fs::symlink(&outside, &link).unwrap();
    let oversize = fixture.worktree.join(".orbit/tmp/oversize.json");
    std::fs::write(&oversize, vec![b' '; 1024 * 1024 + 1]).unwrap();
    let broker = fixture.serve("agent_implement");
    for path in [link, oversize] {
        refused(fixture.call(
            Some(broker.socket_path()),
            PUT,
            json!({"id": TASK, "path": "evidence.json", "source_path": path}),
        ));
    }
    assert!(fixture.owner.calls.lock().unwrap().is_empty());
}
