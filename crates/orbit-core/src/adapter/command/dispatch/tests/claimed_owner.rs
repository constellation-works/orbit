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
use orbit_types::workflow::{
    OWNER_ROUTE_UNAVAILABLE_ERROR_CODE, REVIEW_EVIDENCE_HOLD_ARTIFACT, REVIEW_GATE_ARTIFACT,
    REVIEW_MANIFEST_ARTIFACT, REVIEW_REPORT_ARTIFACT, REVIEW_REPORT_HISTORY_ARTIFACT,
};
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

fn finding() -> Value {
    json!([{"type": "spawned_from", "target": TASK},
           {"type": "regression_from", "target": "TSO-2"}])
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
    fixture.owner.hold("notes.md", b"notes");
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
        // The broker holds no copy of the claimed task, so a finding's
        // `regression_from` crosses it beside `spawned_from`; the owner decides
        // whether the claimed task files findings [ORB-14792].
        (
            ADD,
            json!({"title": "Finding", "description": "Found in review.",
                   "complexity": "low", "relations": finding(), "model": "codex"}),
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
    assert_eq!(received[2].1["relations"], finding());
    let put = &received[5].1;
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
            "a finding not spawned from the claimed task",
            ADD,
            json!({"title": "t", "description": "d", "complexity": "low",
                   "relations": [{"type": "regression_from", "target": "TSO-2"}]}),
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
            "a path outside the task's artifacts",
            GET,
            json!({"id": TASK, "path": "../TSO-2/notes.md"}),
        ),
        (
            "the gate's report outside the reviewer",
            PUT,
            json!({"id": TASK, "path": REVIEW_REPORT_ARTIFACT, "source_path": report}),
        ),
    ];
    for (case, name, input) in cases {
        let error = match fixture.call(Some(broker.socket_path()), name, input) {
            Some(ClaimedOwnerRoute::Forwarded(Err(error)))
            | Some(ClaimedOwnerRoute::Refused(error)) => error.to_string(),
            other => panic!("{case} must be refused before reaching the owner: {other:?}"),
        };
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
fn a_local_claimed_task_read_stays_on_the_claimed_task() {
    let fixture = Fixture::new();
    let local_owner = WorkerInvocation {
        owner_machine_id: FOLLOWER.into(),
        ..fixture.binding.clone()
    };
    for input in [
        json!({"id": "TSO-2"}),
        json!({"id": TASK, "_worker_read": "tasks"}),
    ] {
        let error = refused(bridge_through(
            None,
            &fixture.global_root,
            Some(&local_owner),
            Some(FOLLOWER),
            SHOW,
            &input,
            &fixture.worktree,
            &fixture.worktree,
            ToolEntryPoint::Cli,
        ));
        assert!(
            matches!(&error, OrbitError::PolicyDenied(reason)
                if reason.starts_with("claimed_owner_bridge_refused")),
            "{error}"
        );
    }
    assert!(
        bridge_through(
            None,
            &fixture.global_root,
            Some(&local_owner),
            Some(FOLLOWER),
            SHOW,
            &json!({"id": TASK}),
            &fixture.worktree,
            &fixture.worktree,
            ToolEntryPoint::Cli,
        )
        .is_none(),
        "the claimed task remains readable through the local owner route"
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

/// A call sent to the broker's socket as the sandboxed agent itself can send
/// it, bypassing the nested `orbit` and whatever it would prepare.
fn raw_call(
    fixture: &Fixture,
    socket: &std::path::Path,
    name: &str,
    input: Value,
) -> Result<Value, String> {
    crate::runtime::plugin::broker::forward_call_with_status(
        socket,
        name,
        input,
        &fixture.worktree,
        None,
        "cli",
    )
    .map_err(|error| match error {
        crate::runtime::plugin::broker::ForwardCallError::BrokerAudit(error)
        | crate::runtime::plugin::broker::ForwardCallError::CallerAudit(error) => error.to_string(),
    })
}

/// The broker decides on the artifact path the owner would store, not the
/// raw string: ` review-gate.json` is stored as `review-gate.json`, so a
/// check on the raw name let a non-reviewer forge the gate's certificate.
#[test]
fn a_review_artifact_path_in_any_spelling_is_refused_to_a_non_reviewer_without_reaching_the_owner()
{
    use base64::Engine as _;

    let fixture = Fixture::new();
    let broker = fixture.serve("agent_implement");
    let forged = base64::engine::general_purpose::STANDARD.encode(br#"{"verdict":"approve"}"#);
    let spellings = |name: &str| {
        [
            format!(" {name}"),
            format!("{name} "),
            format!("\t{name}"),
            format!("{name}\t"),
            format!("\n{name}"),
            format!("{name}\n"),
            format!("./{name}"),
            format!("./ {name}"),
            format!(" ./\t./{name}/"),
            name.to_ascii_uppercase(),
        ]
    };
    for name in [
        REVIEW_GATE_ARTIFACT,
        REVIEW_MANIFEST_ARTIFACT,
        REVIEW_EVIDENCE_HOLD_ARTIFACT,
        REVIEW_REPORT_ARTIFACT,
        REVIEW_REPORT_HISTORY_ARTIFACT,
    ] {
        for path in spellings(name) {
            for (tool, input) in [
                (
                    PUT,
                    json!({"id": TASK, "path": path, "content_base64": forged}),
                ),
                (GET, json!({"id": TASK, "path": path})),
            ] {
                let refused = raw_call(&fixture, broker.socket_path(), tool, input)
                    .expect_err(&format!("{tool} {path:?} is refused"));
                assert!(
                    refused.contains("claimed_review_bridge_refused")
                        && refused.contains("before-PR reviewer"),
                    "{tool} {path:?} takes the reviewer-only path: {refused}"
                );
            }
        }
    }
    assert!(
        fixture.owner.calls.lock().unwrap().is_empty(),
        "no spelling of a review artifact reaches the owner"
    );

    // An ordinary artifact reaches the owner under the canonical path the
    // broker checked.
    raw_call(
        &fixture,
        broker.socket_path(),
        PUT,
        json!({"id": TASK, "path": " ./notes//./evidence.json\n", "content_base64": forged}),
    )
    .unwrap();
    fixture.owner.hold("notes/evidence.json", b"{}");
    raw_call(
        &fixture,
        broker.socket_path(),
        GET,
        json!({"id": TASK, "path": "\tnotes//evidence.json"}),
    )
    .unwrap();
    let calls = fixture.owner.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].1["artifacts"][0]["path"], "notes/evidence.json");
    assert_eq!(calls[1].1["path"], "notes/evidence.json");
}

/// The running reviewer's attempt scope sees the canonical path too: a
/// spelled-out gate artifact is refused, and its report is attached under
/// the contract's own name.
#[test]
fn the_reviewer_scope_decides_on_and_forwards_the_canonical_path() {
    use base64::Engine as _;

    let fixture = Fixture::new();
    let broker = fixture.serve("agent_review_repair");
    let (_, report) = fixture.report("report.json", &fixture.attempt_id);
    let encoded = base64::engine::general_purpose::STANDARD.encode(&report);
    for path in [
        format!(" {REVIEW_GATE_ARTIFACT}"),
        format!("\t{REVIEW_MANIFEST_ARTIFACT}"),
        format!("{REVIEW_EVIDENCE_HOLD_ARTIFACT}\n"),
    ] {
        let refused = raw_call(
            &fixture,
            broker.socket_path(),
            PUT,
            json!({"id": TASK, "path": path, "content_base64": encoded}),
        )
        .unwrap_err();
        assert!(
            refused.contains(&format!("carries only `{REVIEW_REPORT_ARTIFACT}`")),
            "{path:?}: {refused}"
        );
    }
    assert!(fixture.owner.puts().is_empty());

    raw_call(
        &fixture,
        broker.socket_path(),
        PUT,
        json!({"id": TASK, "path": format!(" ./{REVIEW_REPORT_ARTIFACT}\t"), "content_base64": encoded}),
    )
    .unwrap();
    let puts = fixture.owner.puts();
    assert_eq!(puts.len(), 1);
    assert_eq!(puts[0]["artifacts"][0]["path"], REVIEW_REPORT_ARTIFACT);
}
