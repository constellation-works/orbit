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
            host_id: None,
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
