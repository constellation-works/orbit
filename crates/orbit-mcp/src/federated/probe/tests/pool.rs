//! Pooling of routed destination sessions against fake child destinations.
//!
//! These drive the private session/pool seam for the same reason
//! [`super::session`] does: reuse safety is a property of the session's wire
//! state — answered, unread, wedged — which the public mux API cannot
//! inject. The behaviors asserted are the public ones: a healthy session is
//! reused for the next routed call with no second handshake, and a session
//! left mid-call or wedged is discarded instead of reused (the reverted
//! pool's hang).

use super::super::contracts::RoutedSession;
use super::super::pool::{RoutedSessionKey, RoutedSessionPool};
use super::super::session::DestinationSession;
use super::super::ssh::SshRoutedSession;
use crate::federated::config::Destination;
use crate::remote::McpSessionAuthority;
use orbit_common::OrbitError;
use orbit_types::tool::ToolSessionContext;
use serde_json::json;
use std::process::{Command, Stdio};
use std::time::Duration;

const OWNER_MACHINE: &str = "hm_owner";

/// A destination whose behavior turns on `mode`:
/// - `healthy`: answers everything, journaling handshake/call counts.
/// - `never-answers-call`: consumes a `tools/call` and stays silent.
/// - `wedged-after-call`: answers one call, then never answers a `ping`.
fn fake_destination_session(mode: &str) -> DestinationSession {
    let script = r#"
import json
import sys

mode = sys.argv[1]
initializes = 0
calls = 0
for line in sys.stdin:
    request = json.loads(line)
    if 'id' not in request:
        continue
    method = request['method']
    if method == 'initialize':
        initializes += 1
        result = {'protocolVersion': request['params']['protocolVersion']}
    elif method == 'ping':
        if mode == 'wedged-after-call' and calls >= 1:
            continue
        result = {}
    elif method == 'tools/call':
        if mode == 'never-answers-call':
            continue
        calls += 1
        result = {'structuredContent': {'initializes': initializes, 'calls': calls}}
    elif method == 'tools/list':
        result = {'tools': []}
    else:
        continue
    print(json.dumps({'jsonrpc': '2.0', 'id': request['id'], 'result': result}), flush=True)
"#;
    let child = Command::new("python3")
        .args(["-u", "-c", script, mode])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn a fake destination");
    let mut session = DestinationSession::start(
        Destination::ssh("orbit-owner", OWNER_MACHINE),
        child,
        Duration::from_secs(5),
    )
    .expect("start a session against the fake destination");
    session.handshake().expect("handshake with the destination");
    session
}

fn pool_key(orchestrator: Option<&str>) -> RoutedSessionKey {
    RoutedSessionKey {
        machine_id: OWNER_MACHINE.to_string(),
        ssh_target: "orbit-owner".to_string(),
        caller_machine_id: "hm_caller".to_string(),
        orchestrator: orchestrator.map(ToOwned::to_owned),
        authority: McpSessionAuthority::Agent,
        internal: false,
        worker_host: false,
        worker_invocation: None,
        line_cap: None,
    }
}

/// One routed call on a pool-returning session, dropped at the end so an
/// intact session parks exactly as a completed routed call's would.
fn routed_call(
    pool: &RoutedSessionPool,
    key: RoutedSessionKey,
    session: DestinationSession,
    delivery_timeout: Duration,
) -> Result<serde_json::Value, OrbitError> {
    let mut routed =
        SshRoutedSession::new(session, delivery_timeout).returning_to(pool.clone(), key);
    routed.call_tool(
        "orbit.task.add",
        json!({"request_id": "pool-test"}),
        ToolSessionContext::default(),
    )
}

#[test]
fn a_healthy_session_is_reused_for_the_next_routed_call_without_a_new_handshake() {
    let pool = RoutedSessionPool::default();
    let key = pool_key(None);
    let first = routed_call(
        &pool,
        key.clone(),
        fake_destination_session("healthy"),
        Duration::from_secs(5),
    )
    .expect("first routed call");
    assert_eq!(first, json!({"initializes": 1, "calls": 1}));

    let mut reused = pool
        .checkout(&key)
        .expect("the intact session must be parked for the next call");
    assert!(
        pool.checkout(&key).is_none(),
        "checkout is exclusive: a second in-flight call never shares the session"
    );
    assert!(
        reused.health_check_for_reuse(Duration::from_secs(5)),
        "a healthy parked session passes its reuse health check"
    );
    reused.rearm_for_reuse(Duration::from_secs(5));
    let second = routed_call(&pool, key, reused, Duration::from_secs(5)).expect("reused call");
    assert_eq!(
        second,
        json!({"initializes": 1, "calls": 2}),
        "the reused session serves the second call with no second initialize"
    );
}

#[test]
fn a_session_left_mid_call_is_discarded_instead_of_parked() {
    let pool = RoutedSessionPool::default();
    let key = pool_key(None);
    let error = routed_call(
        &pool,
        key.clone(),
        fake_destination_session("never-answers-call"),
        Duration::from_millis(300),
    )
    .expect_err("the destination never answers the dispatched call");
    assert!(
        matches!(error, OrbitError::OutcomeUnknown { .. }),
        "a lost dispatched answer stays outcome_unknown: {error}"
    );
    assert!(
        pool.checkout(&key).is_none(),
        "a session whose call timed out unanswered must never be parked or reused"
    );
}

#[test]
fn a_wedged_parked_session_fails_its_health_check_and_is_not_reused() {
    let pool = RoutedSessionPool::default();
    let key = pool_key(None);
    routed_call(
        &pool,
        key.clone(),
        fake_destination_session("wedged-after-call"),
        Duration::from_secs(5),
    )
    .expect("the call the destination answers before wedging");

    let mut parked = pool
        .checkout(&key)
        .expect("the intact answered session parks before it wedges");
    assert!(
        !parked.health_check_for_reuse(Duration::from_millis(300)),
        "a parked session that no longer answers ping must be discarded, not reused"
    );
}

#[test]
fn parking_keeps_at_most_one_session_per_destination() {
    let pool = RoutedSessionPool::default();
    let first_key = pool_key(None);
    let second_key = pool_key(Some("other-orchestrator"));
    routed_call(
        &pool,
        first_key.clone(),
        fake_destination_session("healthy"),
        Duration::from_secs(5),
    )
    .expect("first destination call");
    routed_call(
        &pool,
        second_key.clone(),
        fake_destination_session("healthy"),
        Duration::from_secs(5),
    )
    .expect("second destination call under another key");

    assert!(
        pool.checkout(&first_key).is_none(),
        "parking the second session closes the first: one parked runtime per destination"
    );
    assert!(
        pool.checkout(&second_key).is_some(),
        "the most recently parked session for the destination survives"
    );
}
