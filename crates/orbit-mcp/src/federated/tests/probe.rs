//! Delivery budget and post-dispatch classification.
//!
//! The fake destination here is a child process that holds the pipe open and
//! never writes a line, so a real [`DestinationSession`] runs its own timeout
//! path without an SSH host: everything the mux sends is accepted and nothing
//! is ever answered.

use std::io::Cursor;
use std::process::{Command, Stdio};
use std::sync::atomic::AtomicU64;
use std::time::{Duration, Instant};

use orbit_common::OrbitError;
use orbit_types::tool::ToolSessionContext;
use serde_json::json;

use super::super::probe::{
    BoundedLine, DestinationSession, RoutedSession, SshRoutedSession, read_bounded_line,
};
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

/// A destination that answers `initialize` with a JSON line of `oversize`
/// bytes, then answers `tools/call` with the same. The bulk sits in a string
/// field so every line is valid JSON: only its length can be wrong.
fn bulky_destination(oversize: usize) -> std::process::Child {
    Command::new("python3")
        .args([
            "-u",
            "-c",
            r#"
import sys, json
size = int(sys.argv[1])
for line in sys.stdin:
    request = json.loads(line)
    if "id" not in request:
        continue
    if request["method"] == "initialize":
        result = {"protocolVersion": "2025-06-18", "pad": "a" * size}
    else:
        result = {"structuredContent": {"blob": "a" * size}}
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}) + "\n")
    sys.stdout.flush()
"#,
            &oversize.to_string(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn a bulky destination")
}

const FIVE_MIB: usize = 5 * 1024 * 1024;

#[test]
fn an_oversized_handshake_answer_is_refused_with_a_line_limit_error() {
    let mut session = DestinationSession::start(
        destination("orbit-owner", OWNER_MACHINE),
        bulky_destination(FIVE_MIB),
        Duration::from_secs(20),
    )
    .expect("start a session against the bulky destination");

    let started = Instant::now();
    let error = session
        .handshake()
        .expect_err("a handshake answer past the probe ceiling is not a handshake");
    assert!(
        matches!(error, OrbitError::UnreachableDestination(_)),
        "{error}"
    );
    assert!(
        error.to_string().contains("line limit"),
        "the refusal must say why, not read as a silent disconnect: {error}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the oversized line is refused when it crosses the cap, not at the deadline"
    );
}

#[test]
fn a_routed_tool_result_may_exceed_the_probe_ceiling() {
    // Sessions start at the probe ceiling; only delivery raises it, so a
    // 5 MiB result proves tool results keep their own, larger, cap.
    let mut session = DestinationSession::start(
        destination("orbit-owner", OWNER_MACHINE),
        bulky_destination(FIVE_MIB),
        Duration::from_secs(20),
    )
    .expect("start a session against the bulky destination");
    let reply = session
        .call_tool("orbit.task.list", json!({}))
        .expect("a large tool result is legitimate and must be delivered");
    assert_eq!(reply["blob"].as_str().map(str::len), Some(FIVE_MIB));
}

#[test]
fn bounded_line_reader_splits_lines_and_refuses_an_overlong_one() {
    let cap = AtomicU64::new(8);
    let mut reader = Cursor::new(b"one\ntwo\n12345678\n123456789\nlast".to_vec());

    let mut next = || read_bounded_line(&mut reader, &cap).expect("read");
    assert_eq!(next(), BoundedLine::Line("one\n".into()));
    assert_eq!(next(), BoundedLine::Line("two\n".into()));
    // Newline included, this line is exactly nine bytes: over an 8-byte cap.
    assert_eq!(next(), BoundedLine::TooLong { limit: 8 });
}

#[test]
fn bounded_line_reader_yields_a_final_unterminated_line_then_eof() {
    let cap = AtomicU64::new(64);
    let mut reader = Cursor::new(b"tail".to_vec());

    assert_eq!(
        read_bounded_line(&mut reader, &cap).expect("read"),
        BoundedLine::Line("tail".into())
    );
    assert_eq!(
        read_bounded_line(&mut reader, &cap).expect("read"),
        BoundedLine::Eof
    );
}

#[test]
fn bounded_line_reader_rejects_invalid_utf8() {
    let cap = AtomicU64::new(64);
    let mut reader = Cursor::new(vec![0xff, 0xfe, b'\n']);

    assert!(read_bounded_line(&mut reader, &cap).is_err());
}

#[test]
fn an_internal_preflight_stall_is_unreachable_before_admission() {
    let mut session = SshRoutedSession::new(
        stalled_session(Duration::from_millis(50)),
        Duration::from_secs(1),
    );
    assert!(matches!(
        session.internal_drain_protocol(),
        Err(OrbitError::UnreachableDestination(_))
    ));
}

#[test]
fn a_lost_internal_admission_answer_is_outcome_unknown() {
    let mut session = stalled_session(Duration::from_millis(50));
    let error = session
        .call_internal_drain(
            "orbit.task.pull",
            json!({"request_id":"original-admission"}),
        )
        .expect_err("no reply");
    assert!(
        matches!(error, OrbitError::OutcomeUnknown { .. }),
        "a sent internal admission may have committed: {error}"
    );
}
