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

/// Force peer traffic between a consumed call and its actual answer. The
/// private session seam admits deterministic protocol fault injection without
/// replacing SSH globally or mutating the executor's Orbit state.
fn peer_request_session(scenario: &str) -> DestinationSession {
    let script = r#"
import json
import sys
import time

def send(message):
    print(json.dumps(message), flush=True)

def exchange(peer_id, method, code=None):
    send({'jsonrpc': '2.0', 'id': peer_id, 'method': method})
    reply = json.loads(sys.stdin.readline())
    assert reply['jsonrpc'] == '2.0' and reply['id'] == peer_id, reply
    assert 'method' not in reply, reply
    if code is None:
        assert reply['result'] == {} and 'error' not in reply, reply
    else:
        assert reply['error']['code'] == code and 'result' not in reply, reply

for line in sys.stdin:
    request = json.loads(line)
    if 'id' not in request:
        continue
    call_id = request['id']
    if request['method'] == 'initialize':
        send({'jsonrpc': '2.0', 'id': call_id,
              'result': {'protocolVersion': request['params']['protocolVersion']}})
        continue
    assert request['method'] in ('tools/call', 'orbit/internal/drain/call'), request
    scenario = sys.argv[1]
    if scenario == 'exchange':
        exchange(call_id, 'ping')
        exchange(call_id + 100, 'ping')
        exchange('peer-ping', 'ping')
        exchange(4.5, 'ping')
        exchange(None, 'ping')
        exchange(call_id, 'roots/list', -32601)
        exchange('peer-unsupported', 'sampling/createMessage', -32601)
        exchange('peer-invalid', 42, -32600)
        # Even forged result fields must not turn a notification into a reply.
        send({'jsonrpc': '2.0', 'method': 'notifications/progress',
              'result': {'structuredContent': {'wrong': 'notification'}}})
        send({'jsonrpc': '2.0', 'id': call_id + 100,
              'result': {'structuredContent': {'wrong': 'unrelated reply'}}})
        send({'jsonrpc': '2.0', 'id': call_id + 200,
              'error': {'code': -32603, 'message': 'unrelated error'}})
    elif scenario in ('peer-flood', 'notification-flood'):
        until = time.monotonic() + 3
        while time.monotonic() < until:
            if scenario == 'peer-flood':
                exchange(call_id, 'ping')
            else:
                send({'jsonrpc': '2.0', 'method': 'notifications/progress'})
    elif scenario == 'blocked-peer-write':
        # The echoed string ID exceeds pipe capacity; stop reading the reply.
        send({'jsonrpc': '2.0', 'id': 'x' * (1024 * 1024), 'method': 'ping'})
        time.sleep(30)
    else:
        malformed = {'jsonrpc': '2.0', 'id': call_id}
        if scenario == 'both-result-and-error':
            malformed.update(result={}, error={'code': -32603, 'message': 'bad'})
        elif scenario == 'wrong-version':
            malformed.update(jsonrpc='1.0', result={})
        send(malformed)
        continue
    send({'jsonrpc': '2.0', 'id': call_id, 'result': {
        'structuredContent': {'actual': True, 'arguments': request['params']['arguments']}}})
"#;
    let child = Command::new("python3")
        .args(["-u", "-c", script, scenario])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn a peer-request destination");
    DestinationSession::start(
        Destination::ssh("orbit-owner", OWNER_MACHINE),
        child,
        Duration::from_secs(5),
    )
    .expect("start a peer-request session")
}

#[test]
fn peer_requests_are_answered_before_correlating_the_actual_call_response() {
    for internal in [false, true] {
        let mut session = peer_request_session("exchange");
        session.handshake().expect("initialize destination");
        let arguments = json!({"request_id": "original-call"});
        let result = if internal {
            session.call_internal_drain("orbit.task.pull", arguments.clone())
        } else {
            session.call_tool("orbit.task.add", arguments.clone())
        }
        .expect("peer requests must not consume the outgoing call's response");
        assert_eq!(result, json!({"actual": true, "arguments": arguments}));
    }
}

#[test]
fn peer_traffic_and_blocked_peer_replies_preserve_the_call_deadline_and_unknown_outcome() {
    for scenario in ["peer-flood", "notification-flood", "blocked-peer-write"] {
        let mut session = peer_request_session(scenario);
        session.handshake().expect("initialize destination");
        session.restart_budget(Duration::from_millis(350));
        let started = std::time::Instant::now();
        let error = session
            .call_tool("orbit.task.add", json!({}))
            .expect_err("peer traffic must not complete or extend the dispatched call");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{scenario}: servicing peer traffic must retain the original deadline"
        );
        let OrbitError::OutcomeUnknown { mcp_call_id, .. } = error else {
            panic!("{scenario}: a lost post-dispatch reply must retain ambiguity: {error}");
        };
        assert_eq!(mcp_call_id, format!("{OWNER_MACHINE}/orbit.task.add#2"));
    }
}

#[test]
fn only_response_shaped_messages_can_complete_a_dispatched_call() {
    for scenario in ["missing-result", "both-result-and-error", "wrong-version"] {
        let mut session = peer_request_session(scenario);
        session.handshake().expect("initialize destination");
        let error = session
            .call_tool("orbit.task.add", json!({}))
            .expect_err("a matching ID alone does not make a valid JSON-RPC response");
        assert!(
            matches!(error, OrbitError::OutcomeUnknown { .. }),
            "{scenario}: malformed post-dispatch replies must retain ambiguity: {error}"
        );
    }
}
