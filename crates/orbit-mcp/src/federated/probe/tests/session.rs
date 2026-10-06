//! Fault injection for phase-specific malformed-reply classification.
//!
//! A child destination consumes requests before corrupting their replies. This
//! requires the real session I/O seam, which the public mux API cannot expose.

use super::super::session::DestinationSession;
use crate::federated::config::Destination;
use orbit_common::OrbitError;
use serde_json::json;
use std::process::{Command, Stdio};
use std::time::Duration;

const OWNER_MACHINE: &str = "hm_owner";

/// Record each consumed request before corrupting its reply. A subsequent
/// `tools/list` acts as a fence and returns the journal, proving whether a
/// mutation was replayed without sleeps or shared filesystem state.
fn malformed_reply_session(method: &str) -> DestinationSession {
    let script = r#"
import json
import sys

requests = []
for line in sys.stdin:
    request = json.loads(line)
    if 'id' not in request:
        continue
    method = request['method']
    if method == 'tools/list':
        result = {'tools': [{'name': 'journal', 'inputSchema': {'type': 'object'},
                             '_meta': {'requests': requests}}]}
    elif method == sys.argv[1]:
        requests.append(request)
        print('{"jsonrpc":', flush=True)
        continue
    elif method == 'initialize':
        result = {'protocolVersion': request['params']['protocolVersion']}
    else:
        raise RuntimeError('unexpected request: ' + method)
    print(json.dumps({'jsonrpc': '2.0', 'id': request['id'], 'result': result}), flush=True)
"#;
    let child = Command::new("python3")
        .args(["-u", "-c", script, method])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn a destination that corrupts a consumed request's reply");
    DestinationSession::start(
        Destination::ssh("orbit-owner", OWNER_MACHINE),
        child,
        Duration::from_secs(5),
    )
    .expect("start a session against the malformed-reply destination")
}

#[test]
fn malformed_mutation_replies_preserve_unknown_outcome_identity_without_replay() {
    for (method, tool) in [
        ("tools/call", "orbit.task.add"),
        (crate::internal_drain::CALL_METHOD, "orbit.task.pull"),
    ] {
        let mut session = malformed_reply_session(method);
        session
            .handshake()
            .expect("valid handshake before dispatch");
        let arguments = json!({"request_id": "original-mutation"});
        let error = if method == "tools/call" {
            session.call_tool(tool, arguments.clone())
        } else {
            session.call_internal_drain(tool, arguments.clone())
        }
        .expect_err("the destination consumed the mutation but corrupted its answer");

        // Fence the destination's input before inspecting the journal so a
        // second effect cannot hide behind a scheduling race or teardown.
        let tools = session
            .list_tool_definitions()
            .expect("read the consumed-request journal");
        let requests = tools[0]["_meta"]["requests"]
            .as_array()
            .expect("journal contains requests");
        assert_eq!(
            requests.len(),
            1,
            "{method}: a malformed post-dispatch answer must not replay the mutation"
        );
        let request = &requests[0];
        assert_eq!(request["method"], method);
        let wire_name = if method == "tools/call" {
            orbit_types::tool::mcp_advertised_tool_name(tool)
        } else {
            tool.to_string()
        };
        assert_eq!(request["params"]["name"], wire_name);
        assert_eq!(request["params"]["arguments"], arguments);
        if method != "tools/call" {
            assert_eq!(
                request["params"]["protocol"],
                crate::INTERNAL_DRAIN_PROTOCOL
            );
        }
        let OrbitError::OutcomeUnknown {
            mcp_call_id,
            message,
        } = error
        else {
            panic!("{method}: a consumed mutation must preserve outcome ambiguity: {error}");
        };
        let request_id = request["id"]
            .as_i64()
            .expect("destination request identity");
        assert_eq!(mcp_call_id, format!("{OWNER_MACHINE}/{tool}#{request_id}"));
        assert!(
            message.contains(method),
            "retain the request method: {message}"
        );
        assert!(
            message.contains("invalid JSON") && message.contains("line 1 column"),
            "retain JSON parser diagnostics: {message}"
        );
    }
}

#[test]
fn malformed_probe_replies_remain_unreachable_before_mutating_dispatch() {
    for method in ["initialize", "tools/call"] {
        let mut session = malformed_reply_session(method);
        let error = if method == "initialize" {
            session.handshake()
        } else {
            session
                .handshake()
                .expect("valid handshake before discovery");
            session.discover_workspaces(json!({})).map(|_| ())
        }
        .expect_err("the read-only probe received malformed JSON");
        let OrbitError::UnreachableDestination(message) = error else {
            panic!("{method}: a failed probe must remain unreachable: {error}");
        };
        assert!(
            message.contains(OWNER_MACHINE),
            "retain destination identity: {message}"
        );
        assert!(
            message.contains("invalid JSON") && message.contains("line 1 column"),
            "retain JSON parser diagnostics: {message}"
        );
    }
}
