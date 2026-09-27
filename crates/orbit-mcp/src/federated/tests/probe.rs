//! Delivery budget and post-dispatch classification.
//!
//! The fake destination here is a child process that holds the pipe open and
//! never writes a line, so a real [`DestinationSession`] runs its own timeout
//! path without an SSH host: everything the mux sends is accepted and nothing
//! is ever answered.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use orbit_common::OrbitError;
use orbit_types::tool::ToolSessionContext;
use serde_json::json;

use super::super::probe::{DestinationSession, RoutedSession, SshRoutedSession};
use super::fixtures::{OWNER_MACHINE, destination};

/// A destination that takes the request and stops there.
fn stalled_session(budget: Duration) -> DestinationSession {
    let child = Command::new("sleep")
        .arg("30")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn a stalled destination");
    DestinationSession::start(destination("orbit-owner", OWNER_MACHINE), child, budget)
        .expect("start a session against the stalled destination")
}

#[test]
fn a_stall_before_the_tool_call_is_an_unreachable_destination() {
    let mut session = stalled_session(Duration::from_millis(50));

    for (phase, error) in [
        ("initialize", session.handshake().err()),
        ("discovery", session.discover_workspaces().err()),
        ("tools/list", session.list_tools().err()),
    ] {
        let error = error.unwrap_or_else(|| panic!("{phase} cannot complete against a stall"));
        assert!(
            matches!(error, OrbitError::UnreachableDestination(_)),
            "nothing the caller asked for has run yet at {phase}: {error}"
        );
    }
}

#[test]
fn a_stall_after_the_tool_call_is_dispatched_is_outcome_unknown() {
    let mut session = stalled_session(Duration::from_millis(50));

    let error = session
        .call_tool("orbit.task.add", json!({ "title": "remote write" }))
        .expect_err("the destination never answers the dispatched call");

    match error {
        OrbitError::OutcomeUnknown {
            mcp_call_id,
            message,
        } => {
            assert!(
                mcp_call_id.starts_with(&format!("{OWNER_MACHINE}/orbit.task.add#")),
                "the ambiguous call names the destination-facing request: {mcp_call_id}"
            );
            assert!(
                message.contains("may have completed"),
                "the message must not read as a delivery miss: {message}"
            );
        }
        other => panic!(
            "a written tools/call that timed out may already have committed remotely, so it \
             cannot be reported as an unreachable destination: {other}"
        ),
    }
}

#[test]
fn routed_delivery_does_not_inherit_an_exhausted_probe_budget() {
    let delivery = Duration::from_millis(300);

    // A zero probe budget is the limit case of what SSH setup, the handshake,
    // discovery, and tools/list leave behind on a slow destination.
    let mut exhausted = stalled_session(Duration::ZERO);
    let started = Instant::now();
    exhausted
        .list_tools()
        .expect_err("an exhausted probe budget gives up at once");
    assert!(
        started.elapsed() < delivery,
        "the probe budget really is spent, so the contrast below is meaningful"
    );

    let mut routed = SshRoutedSession::new(stalled_session(Duration::ZERO), delivery);
    let started = Instant::now();
    let error = routed
        .call_tool(
            "orbit.command.exec",
            json!({ "command": "make ci-lint" }),
            ToolSessionContext::default(),
        )
        .expect_err("the destination never answers the dispatched call");
    let waited = started.elapsed();

    assert!(
        waited >= delivery,
        "the tool call gets its own budget rather than what classification left over: \
         gave up after {waited:?}"
    );
    assert!(
        matches!(error, OrbitError::OutcomeUnknown { .. }),
        "{error}"
    );
}

/// A destination that streams garbage as fast as the pipe allows. The reader
/// queue applies backpressure instead of buffering the flood while the
/// consumer decides, and the first non-JSON line ends the session promptly.
#[cfg(unix)]
#[test]
fn a_flooding_destination_is_refused_without_buffering_the_flood() {
    let child = Command::new("yes")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn a flooding destination");
    let mut session = DestinationSession::start(
        destination("orbit-owner", OWNER_MACHINE),
        child,
        Duration::from_secs(5),
    )
    .expect("start a session against the flooding destination");

    let started = Instant::now();
    let error = session
        .handshake()
        .expect_err("a stream of `y` lines is not an MCP answer");
    assert!(
        matches!(error, OrbitError::UnreachableDestination(_)),
        "{error}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "the first bad line must end the session, not the deadline"
    );
}

/// A scripted destination that answers `initialize`, discovery, and
/// `tools/list`, and handles `tools/call` per `on_call`: `answer` replies,
/// `stop_reading` stops draining stdin, and `ignore` never replies. With
/// `chatter`, a background thread streams unrelated valid messages — a
/// notification and an answer to an id nobody asked — for the whole session.
fn scripted_destination(on_call: &str, chatter: bool) -> std::process::Child {
    Command::new("python3")
        .args([
            "-u",
            "-c",
            r#"
import sys, json, threading, time
on_call, chatter, discovery = sys.argv[1], sys.argv[2] == "1", sys.argv[3]
lock = threading.Lock()
def emit(message):
    with lock:
        sys.stdout.write(json.dumps(message) + "\n")
        sys.stdout.flush()
if chatter:
    def stream():
        noise = [
            {"jsonrpc": "2.0", "method": "notifications/message", "params": {"level": "info", "data": "x"}},
            {"jsonrpc": "2.0", "id": 987654, "result": {}},
        ]
        while True:
            for message in noise:
                emit(message)
    threading.Thread(target=stream, daemon=True).start()
for line in sys.stdin:
    request = json.loads(line)
    if "id" not in request:
        continue
    method = request["method"]
    if method == "initialize":
        result = {"protocolVersion": "2025-06-18"}
    elif method == "tools/list":
        result = {"tools": [{"name": "orbit_task_add"}]}
    elif request["params"]["name"] == discovery:
        result = {"structuredContent": {"machine_id": "hm_owner", "workspaces": []}}
    elif on_call == "stop_reading":
        time.sleep(60)
    elif on_call == "ignore":
        continue
    else:
        result = {"structuredContent": {"echo": request["params"]["arguments"]}}
    emit({"jsonrpc": "2.0", "id": request["id"], "result": result})
"#,
            on_call,
            if chatter { "1" } else { "0" },
            crate::FEDERATED_DESTINATION_WORKSPACE_LIST_TOOL,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn a scripted destination")
}

/// Whether `pid` still names a live or unreaped process.
#[cfg(unix)]
fn process_exists(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .expect("run kill -0")
        .success()
}

/// A request larger than any pipe buffer, sent to a destination that stopped
/// reading: the write itself must honor the delivery budget, and the session
/// must still end and reap its child.
#[cfg(unix)]
#[test]
fn a_destination_that_stops_reading_cannot_hold_an_oversized_write_past_the_budget() {
    let delivery = Duration::from_millis(300);
    let child = scripted_destination("stop_reading", false);
    let pid = child.id();
    let mut session = DestinationSession::start(
        destination("orbit-owner", OWNER_MACHINE),
        child,
        Duration::from_secs(5),
    )
    .expect("start a session against the scripted destination");
    session
        .handshake()
        .expect("the destination answers the handshake");
    let mut routed = SshRoutedSession::new(session, delivery);

    // The first call parks the destination; the second is far past what the
    // pipe can hold while nobody drains it.
    routed
        .call_tool(
            "orbit.task.add",
            json!({ "title": "park" }),
            ToolSessionContext::default(),
        )
        .expect_err("the parked destination never answers");
    let started = Instant::now();
    let error = routed
        .call_tool(
            "orbit.task.add",
            json!({ "body": "x".repeat(8 * 1024 * 1024) }),
            ToolSessionContext::default(),
        )
        .expect_err("an undrained oversized request cannot be delivered");
    let waited = started.elapsed();

    assert!(
        waited < Duration::from_secs(3),
        "the write must give up at the delivery budget, not when the destination does: \
         waited {waited:?}"
    );
    assert!(
        matches!(error, OrbitError::UnreachableDestination(_)),
        "a request that never fully left was not delivered: {error}"
    );
    let released = Instant::now();
    drop(routed);
    assert!(
        released.elapsed() < Duration::from_secs(3),
        "dropping the session must not wait on the destination"
    );
    assert!(!process_exists(pid), "the destination child must be reaped");
}

/// Unrelated valid messages keep arriving faster than the deadline: they are
/// skipped while the real answers are matched by id, and they cannot keep a
/// read going once the budget is spent.
#[test]
fn a_chattering_destination_still_answers_and_cannot_extend_the_deadline() {
    let mut session = DestinationSession::start(
        destination("orbit-owner", OWNER_MACHINE),
        scripted_destination("answer", true),
        Duration::from_secs(10),
    )
    .expect("start a session against the chattering destination");
    session.handshake().expect("handshake through the chatter");
    let snapshot = session
        .discover_workspaces()
        .expect("discovery through the chatter");
    assert_eq!(snapshot.machine_id, OWNER_MACHINE);
    assert_eq!(
        session
            .list_tools()
            .expect("tools/list through the chatter"),
        vec!["orbit_task_add".to_string()]
    );
    let mut routed = SshRoutedSession::new(session, Duration::from_secs(10));
    let reply = routed
        .call_tool(
            "orbit.task.add",
            json!({ "title": "routed" }),
            ToolSessionContext::default(),
        )
        .expect("routed call through the chatter");
    assert_eq!(reply["echo"]["title"], "routed");

    let delivery = Duration::from_millis(300);
    let mut session = DestinationSession::start(
        destination("orbit-owner", OWNER_MACHINE),
        scripted_destination("ignore", true),
        Duration::from_secs(10),
    )
    .expect("start a session against the chattering destination");
    session.handshake().expect("handshake through the chatter");
    let mut routed = SshRoutedSession::new(session, delivery);
    let started = Instant::now();
    let error = routed
        .call_tool(
            "orbit.task.add",
            json!({ "title": "remote write" }),
            ToolSessionContext::default(),
        )
        .expect_err("the destination never answers the dispatched call");
    let waited = started.elapsed();

    assert!(
        waited < Duration::from_secs(3),
        "unrelated messages must not keep the read alive past the budget: waited {waited:?}"
    );
    assert!(
        matches!(error, OrbitError::OutcomeUnknown { .. }),
        "the written call may have committed: {error}"
    );
}

#[test]
fn ssh_session_preserves_worker_binding_outside_tool_arguments() {
    let binding = orbit_types::tool::WorkerInvocation {
        owner_machine_id: OWNER_MACHINE.into(),
        owner_workspace_id: "ws_orbit".into(),
        owner_destination: format!("{OWNER_MACHINE}/ws_orbit"),
        task_id: "task".into(),
        claim_id: "claim".into(),
        execution: orbit_types::task::ExecutionLocation {
            machine_id: "executor".into(),
            machine_name: None,
        },
        bound_run_id: "immutable-leaf".into(),
    };
    let child = Command::new("python3")
        .args([
            "-u",
            "-c",
            r#"
import sys,json
binding=None
for line in sys.stdin:
    request=json.loads(line)
    if 'id' not in request: continue
    if request['method']=='initialize':
        binding=request['params']['_meta']['orbit']['worker_invocation']
        result={'protocolVersion':'2025-06-18'}
    else:
        result={'structuredContent':{'binding':binding,'arguments':request['params']['arguments']}}
    print(json.dumps({'jsonrpc':'2.0','id':request['id'],'result':result}),flush=True)
"#,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("fake SSH destination");
    let mut session = DestinationSession::start(
        destination("fixture", OWNER_MACHINE),
        child,
        Duration::from_secs(5),
    )
    .expect("session");
    session
        .handshake_with_worker(Some(&binding))
        .expect("bound handshake");
    let mut route = SshRoutedSession::new(session, Duration::from_secs(5));
    let context = ToolSessionContext {
        worker_invocation: Some(binding.clone()),
        ..Default::default()
    };
    let reply = route
        .call_tool(
            "orbit.task.update",
            json!({"worker_invocation":null}),
            context.clone(),
        )
        .expect("call");
    assert_eq!(
        reply["binding"],
        serde_json::to_value(&binding).expect("binding JSON")
    );
    let mut replacement = context;
    replacement
        .worker_invocation
        .as_mut()
        .expect("binding")
        .bound_run_id = "replacement".into();
    assert!(
        route
            .call_tool("orbit.task.update", json!({}), replacement)
            .is_err()
    );
    assert!(
        route
            .call_tool(
                "orbit.task.update",
                json!({}),
                ToolSessionContext::default()
            )
            .is_err()
    );
}
