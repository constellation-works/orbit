//! Routed-session pooling through the exported SSH probe, with no live host.
//!
//! A scripted `ssh` peer stands in for the destination's `orbit mcp serve`,
//! so every case drives `SshDestinationProbe::open_route` (the production
//! choose / health-check / rediscover / discard / replace composition) and
//! observes only public results plus the peer's own journal: which process
//! served each call, how many handshakes it saw, and whether a discarded
//! session's process is gone. `PATH` is process-wide, so each case re-runs
//! itself in an isolated child that sees the scripted `ssh`.
//!
//! The cases are the reproduced incidents of the reverted pool: a reused
//! session hanging a later call (ORB-11023, ORB-24631) and a parked runtime
//! holding the destination against a fresh one.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use orbit_common::OrbitError;
use orbit_mcp::McpSessionAuthority;
use orbit_mcp::federated::{
    Destination, DestinationProbe, RoutedSession, RoutedSessionPool, SshDestinationProbe,
};
use orbit_types::tool::ToolSessionContext;
use serde_json::{Value, json};

const CHILD: &str = "ORBIT_SSH_POOL_FIXTURE";
const MODE: &str = "ORBIT_SSH_POOL_MODE";

/// The scripted peer. Each `ssh` process is one destination session and
/// journals `<pid> <event>` per request so a case can tell sessions apart:
/// - `healthy`: answers everything.
/// - `never-answers-call`: consumes a routed `tools/call` and stays silent.
/// - `wedged-after-call`: answers one routed call, then never answers `ping`.
/// - `stale-response-before-ping`: answers a `ping` only after re-sending the
///   previous routed call's response, which nobody is awaiting.
const PEER: &str = r#"#!/usr/bin/env python3
import json, os, sys
mode = os.environ['ORBIT_SSH_POOL_MODE']
pid = os.getpid()
journal = os.path.join(os.environ['ORBIT_FIXTURE_DIR'], 'journal')
calls = 0
last_call_id = None


def note(event):
    with open(journal, 'a') as file:
        file.write('%d %s\n' % (pid, event))


for line in sys.stdin:
    request = json.loads(line)
    if 'id' not in request:
        continue
    method = request['method']
    if method == 'initialize':
        note('initialize')
        result = {'protocolVersion': request['params']['protocolVersion']}
    elif method == 'ping':
        note('ping')
        if mode == 'wedged-after-call' and calls >= 1:
            continue
        if mode == 'stale-response-before-ping' and last_call_id is not None:
            print(json.dumps({'jsonrpc': '2.0', 'id': last_call_id, 'result': {'stale': True}}), flush=True)
        result = {}
    elif method == 'tools/call' and request['params']['name'] == 'orbit_federated_destination_workspace_list':
        note('discovery')
        result = {'structuredContent': {'machine_id': 'hm_fixture', 'workspaces': []}}
    elif method == 'tools/call':
        note('call')
        if mode == 'never-answers-call':
            continue
        calls += 1
        last_call_id = request['id']
        result = {'structuredContent': {'pid': pid, 'calls': calls}}
    elif method == 'tools/list':
        result = {'tools': []}
    else:
        continue
    print(json.dumps({'jsonrpc': '2.0', 'id': request['id'], 'result': result}), flush=True)
"#;

/// Run `body` in a child process whose `PATH` starts with the scripted `ssh`
/// peer in `mode`; in the child itself, run it directly.
fn isolated(test: &str, mode: &str, body: fn(&Path)) {
    if let Ok(dir) = std::env::var("ORBIT_FIXTURE_DIR")
        && std::env::var(CHILD).is_ok()
    {
        body(Path::new(&dir));
        return;
    }
    let fixture = tempfile::Builder::new()
        .prefix("orbit-ssh-pool-")
        .tempdir_in("/tmp")
        .unwrap();
    let bin = fixture.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let peer = bin.join("ssh");
    fs::write(&peer, PEER).unwrap();
    fs::set_permissions(&peer, fs::Permissions::from_mode(0o700)).unwrap();
    let output = orbit_common::process::run_bounded(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &format!("ssh_pool::{test}"), "--nocapture"])
            .current_dir(fixture.path())
            .env(CHILD, "1")
            .env(MODE, mode)
            .env("ORBIT_FIXTURE_DIR", fixture.path())
            .env("HOME", fixture.path())
            .env(
                "PATH",
                format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
            ),
        Duration::from_secs(30),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "{test}: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn destination() -> Destination {
    Destination::ssh("fake-peer", "hm_fixture")
}

fn probe(probe_timeout: Duration, delivery_timeout: Duration) -> SshDestinationProbe {
    probe_for(probe_timeout, delivery_timeout, None)
}

fn probe_for(
    probe_timeout: Duration,
    delivery_timeout: Duration,
    orchestrator: Option<&str>,
) -> SshDestinationProbe {
    SshDestinationProbe::new(
        "hm_caller".into(),
        probe_timeout,
        delivery_timeout,
        orchestrator.map(ToOwned::to_owned),
        McpSessionAuthority::Agent,
    )
}

fn open(probe: &SshDestinationProbe) -> Box<dyn RoutedSession> {
    probe
        .open_route(&destination())
        .expect("open a routed session")
}

fn call(route: &mut dyn RoutedSession) -> Result<Value, OrbitError> {
    route.call_tool(
        "orbit.task.add",
        json!({"request_id": "pool-test"}),
        ToolSessionContext::default(),
    )
}

/// One complete routed call; the session is dropped at the end so an intact
/// one parks exactly as a finished call's would.
fn routed_call(probe: &SshDestinationProbe) -> Value {
    call(open(probe).as_mut()).expect("routed call")
}

/// `(pid, event)` for every request the peers received, in order.
fn journal(dir: &Path) -> Vec<(i32, String)> {
    fs::read_to_string(dir.join("journal"))
        .unwrap_or_default()
        .lines()
        .map(|line| {
            let (pid, event) = line.split_once(' ').unwrap();
            (pid.parse().unwrap(), event.to_string())
        })
        .collect()
}

fn count(dir: &Path, event: &str) -> usize {
    journal(dir)
        .iter()
        .filter(|(_, recorded)| recorded == event)
        .count()
}

fn pid(answer: &Value) -> i32 {
    i32::try_from(answer["pid"].as_i64().unwrap()).unwrap()
}

fn process_gone(pid: i32) -> bool {
    // SAFETY: signal zero observes existence without signaling a process.
    let result = unsafe { libc::kill(pid, 0) };
    result == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

#[test]
fn a_healthy_session_is_reused_for_the_next_routed_call_without_a_new_handshake() {
    isolated(
        "a_healthy_session_is_reused_for_the_next_routed_call_without_a_new_handshake",
        "healthy",
        |dir| {
            let probe = probe(Duration::from_secs(5), Duration::from_secs(5));
            let first = routed_call(&probe);
            assert_eq!(first["calls"], 1);
            let second = routed_call(&probe);
            assert_eq!(
                (pid(&second), &second["calls"]),
                (pid(&first), &json!(2)),
                "the reused session serves the second call"
            );
            assert_eq!(
                count(dir, "initialize"),
                1,
                "the reused session serves the second call with no second initialize"
            );
            assert_eq!(count(dir, "ping"), 1, "reuse is gated by one health check");

            // Checkout is exclusive: while one in-flight call holds the
            // parked session, a second never shares it.
            let mut held = open(&probe);
            let mut concurrent = open(&probe);
            let held_answer = call(held.as_mut()).expect("held call");
            let concurrent_answer = call(concurrent.as_mut()).expect("concurrent call");
            assert_eq!(pid(&held_answer), pid(&first));
            assert_ne!(
                pid(&concurrent_answer),
                pid(&first),
                "a second in-flight call never shares the checked-out session"
            );
            assert_eq!(count(dir, "initialize"), 2);
        },
    );
}

#[test]
fn a_session_left_mid_call_is_discarded_instead_of_parked() {
    isolated(
        "a_session_left_mid_call_is_discarded_instead_of_parked",
        "never-answers-call",
        |dir| {
            let probe = probe(Duration::from_secs(5), Duration::from_millis(300));
            let mut route = open(&probe);
            let error = call(route.as_mut())
                .expect_err("the destination never answers the dispatched call");
            assert!(
                matches!(error, OrbitError::OutcomeUnknown { .. }),
                "a lost dispatched answer stays outcome_unknown: {error}"
            );
            drop(route);
            let abandoned = journal(dir)[0].0;
            assert!(
                process_gone(abandoned),
                "a session whose call timed out unanswered is closed, not parked"
            );

            let _next = open(&probe);
            assert_eq!(
                (count(dir, "initialize"), count(dir, "ping")),
                (2, 0),
                "the next call opens a fresh session instead of reusing the abandoned one"
            );
        },
    );
}

#[test]
fn a_wedged_parked_session_fails_its_health_check_and_is_not_reused() {
    isolated(
        "a_wedged_parked_session_fails_its_health_check_and_is_not_reused",
        "wedged-after-call",
        |dir| {
            let probe = probe(Duration::from_millis(1500), Duration::from_secs(5));
            let first = routed_call(&probe);
            let second = routed_call(&probe);
            assert_ne!(
                pid(&second),
                pid(&first),
                "a parked session that no longer answers ping is replaced, not reused"
            );
            assert_eq!(second["calls"], 1);
            assert_eq!(count(dir, "ping"), 1);
            assert_eq!(count(dir, "initialize"), 2);
            assert!(
                process_gone(pid(&first)),
                "the wedged session is closed so it cannot hold the destination"
            );
        },
    );
}

#[test]
fn a_parked_session_with_a_stale_response_is_discarded_during_its_health_check() {
    isolated(
        "a_parked_session_with_a_stale_response_is_discarded_during_its_health_check",
        "stale-response-before-ping",
        |dir| {
            let probe = probe(Duration::from_secs(5), Duration::from_secs(5));
            let first = routed_call(&probe);
            let second = routed_call(&probe);
            assert_ne!(
                pid(&second),
                pid(&first),
                "a response for an earlier request is a framing mismatch and discards the session"
            );
            assert_eq!(second["calls"], 1);
            assert_eq!(count(dir, "ping"), 1);
            assert_eq!(count(dir, "initialize"), 2);
            assert!(process_gone(pid(&first)));
        },
    );
}

#[test]
fn parking_keeps_at_most_one_session_per_destination() {
    isolated(
        "parking_keeps_at_most_one_session_per_destination",
        "healthy",
        |dir| {
            let pool = RoutedSessionPool::default();
            let first_probe = probe(Duration::from_secs(5), Duration::from_secs(5))
                .with_session_pool(pool.clone());
            let second_probe = probe_for(
                Duration::from_secs(5),
                Duration::from_secs(5),
                Some("other-orchestrator"),
            )
            .with_session_pool(pool);

            // Both sessions are in flight, so neither open closes the other;
            // parking is what must keep one runtime per destination.
            let mut first = open(&first_probe);
            let mut second = open(&second_probe);
            let first_pid = pid(&call(first.as_mut()).expect("first destination call"));
            let second_pid = pid(&call(second.as_mut()).expect("second destination call"));
            assert_ne!(first_pid, second_pid);
            drop(first);
            assert!(!process_gone(first_pid), "the first session parks");
            drop(second);
            assert!(
                process_gone(first_pid),
                "parking the second session closes the first: one parked runtime per destination"
            );
            assert!(
                !process_gone(second_pid),
                "the most recently parked session for the destination survives"
            );

            let reused = routed_call(&second_probe);
            assert_eq!(
                (pid(&reused), count(dir, "initialize")),
                (second_pid, 2),
                "the surviving session serves the next call under its own key"
            );
        },
    );
}
