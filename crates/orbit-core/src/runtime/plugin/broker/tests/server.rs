//! The listener end to end over its real socket. Clients are this test
//! process, anchored by ancestry so the checks run without a sandbox; the
//! sandboxed paths are covered in `sandbox.rs`.

use std::io::{ErrorKind, Read, Write};
use std::os::unix::net::UnixStream;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use orbit_common::process::ancestry::process_start_key;
use orbit_engine::PluginBrokerHandle;
use serde_json::{Value, json};
use tempfile::TempDir;

use super::super::PluginBroker;
use super::super::peer::PeerAnchor;
use super::super::protocol::{
    BUSY, INVALID_REQUEST, MAX_REQUEST_BYTES, REFUSED, REQUEST_TOO_LARGE, read_frame, write_frame,
};
use super::super::server::{IN_FLIGHT, IO_TIMEOUT, QUEUED};
use super::{EchoDispatch, REFUSED_TOOL};

const READ_TIMEOUT: Duration = Duration::from_secs(20);

struct Running {
    broker: PluginBroker,
    dispatch: Arc<EchoDispatch>,
    _root: TempDir,
}

fn start(anchor: PeerAnchor) -> Running {
    let root = super::short_tempdir();
    let global = root.path().canonicalize().expect("canonical global root");
    let dispatch = EchoDispatch::shared();
    let broker =
        PluginBroker::start(&global, "run-under-test", dispatch.clone()).expect("start broker");
    broker.bind_anchor(anchor);
    Running {
        broker,
        dispatch,
        _root: root,
    }
}

fn own_anchor() -> PeerAnchor {
    PeerAnchor::Ancestor(process_start_key(std::process::id()).expect("own start key"))
}

fn connect(running: &Running) -> UnixStream {
    let stream = UnixStream::connect(running.broker.socket_path()).expect("connect");
    stream
        .set_read_timeout(Some(READ_TIMEOUT))
        .expect("read timeout");
    stream
}

fn call(running: &Running, request: &[u8]) -> Value {
    let mut stream = connect(running);
    write_frame(&mut stream, request).expect("send request");
    reply(&mut stream)
}

fn reply(stream: &mut UnixStream) -> Value {
    let body = read_frame(stream, MAX_REQUEST_BYTES).expect("broker reply");
    serde_json::from_slice(&body).expect("reply is JSON")
}

fn tool_request() -> Vec<u8> {
    request_for("pulsar.post")
}

fn request_for(tool: &str) -> Vec<u8> {
    json!({"schema_version": 1, "tool": tool, "input": {"text": "hi"}, "cwd": "/work"})
        .to_string()
        .into_bytes()
}

/// True when the broker closed the connection without writing anything.
fn closed_without_reply(stream: &mut UnixStream) -> bool {
    let mut buf = [0u8; 1];
    match stream.read(&mut buf) {
        Ok(0) => true,
        Ok(_) => false,
        Err(error) => matches!(
            error.kind(),
            ErrorKind::ConnectionReset | ErrorKind::BrokenPipe
        ),
    }
}

#[test]
fn an_authenticated_request_is_dispatched_with_its_peer_and_answered_with_the_output() {
    let running = start(own_anchor());

    let response = call(&running, &tool_request());

    let own_pid = std::process::id();
    assert_eq!(
        response,
        json!({
            "schema_version": 1,
            "ok": true,
            "output": {"tool": "pulsar.post", "peer_pid": own_pid},
        })
    );
    let calls = running.dispatch.calls();
    assert_eq!(calls.len(), 1, "one request, one dispatch");
    assert_eq!(calls[0].0.input, json!({"text": "hi"}));
    assert_eq!(
        calls[0].1, own_pid,
        "the dispatch sees the authenticated peer"
    );
}

#[test]
fn a_refused_call_is_answered_with_a_structured_error() {
    let running = start(own_anchor());

    let response = call(&running, &request_for(REFUSED_TOOL));

    assert_eq!(response["ok"], false);
    assert_eq!(response["error"]["code"], REFUSED);
    assert_eq!(response["error"]["retryable"], false);
}

#[test]
fn a_malformed_request_is_answered_invalid_and_never_dispatched() {
    let running = start(own_anchor());

    let response = call(&running, br#"{"schema_version": 1}"#);

    assert_eq!(response["error"]["code"], INVALID_REQUEST);
    assert!(running.dispatch.calls().is_empty());
}

#[test]
fn an_oversized_request_is_refused_from_its_length_prefix_alone() {
    let running = start(own_anchor());
    let mut stream = connect(&running);

    // Only the prefix is sent: the broker must answer without waiting for,
    // or buffering, a body it will not accept.
    stream
        .write_all(&(MAX_REQUEST_BYTES + 1).to_be_bytes())
        .expect("send length prefix");
    let response = reply(&mut stream);

    assert_eq!(response["error"]["code"], REQUEST_TOO_LARGE);
    assert_eq!(response["error"]["retryable"], false);
}

#[test]
fn a_peer_outside_the_sandbox_is_closed_without_a_reply() {
    let mut sibling = Command::new("sleep")
        .arg("30")
        .stdin(Stdio::null())
        .spawn()
        .expect("spawn sibling");
    let anchor = PeerAnchor::Ancestor(process_start_key(sibling.id()).expect("sibling key"));
    let running = start(anchor);

    let mut stream = connect(&running);
    let _ = write_frame(&mut stream, &tool_request());

    assert!(closed_without_reply(&mut stream));
    assert!(
        running.dispatch.calls().is_empty(),
        "a refused peer's request must never reach the dispatch"
    );
    end(&mut sibling);
}

#[test]
fn a_full_broker_answers_busy_and_recovers() {
    let running = start(own_anchor());
    let capacity = IN_FLIGHT + QUEUED;
    let idle: Vec<UnixStream> = (0..capacity).map(|_| connect(&running)).collect();

    let busy = call_when(&running, |response| response["error"]["code"] == BUSY);
    assert_eq!(busy["error"]["retryable"], true, "busy must be retryable");

    drop(idle);
    let recovered = call_when(&running, |response| response["error"]["code"] != BUSY);
    assert_eq!(recovered["ok"], true);
}

/// Call until `accept` holds for the reply. Admission is asynchronous, so the
/// broker may briefly lag the connections this test opened or closed.
fn call_when(running: &Running, accept: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + READ_TIMEOUT;
    loop {
        let mut stream = connect(running);
        // A full broker answers without reading the request. It can close
        // before this write completes; a peer-closed write error does not
        // imply its busy reply was lost. Read that reply from the same connection.
        if let Err(error) = write_frame(&mut stream, &tool_request()) {
            assert!(
                matches!(
                    error.kind(),
                    ErrorKind::BrokenPipe | ErrorKind::NotConnected | ErrorKind::ConnectionReset
                ),
                "send request: {error}"
            );
        }
        let response = reply(&mut stream);
        if accept(&response) || Instant::now() >= deadline {
            return response;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn dropping_the_broker_removes_its_socket_and_directory() {
    let running = start(own_anchor());
    let socket = running.broker.socket_path().to_path_buf();
    let dir = socket.parent().expect("run dir").to_path_buf();
    assert!(socket.exists());

    drop(running.broker);

    assert!(!dir.exists(), "run directory must be removed");
    assert!(UnixStream::connect(&socket).is_err());
}

fn end(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Keep making progress until the server closes, with a bounded writer lifetime
/// even if the assertion fails. All bytes belong to a valid request frame.
fn assert_trickle_expires(prefix_sent: bool, interval: Duration) {
    let running = start(own_anchor());
    let mut stream = connect(&running);
    stream
        .set_read_timeout(Some(IO_TIMEOUT + Duration::from_secs(3)))
        .expect("bounded regression wait");
    let body = tool_request();
    let mut wire = Vec::new();
    write_frame(&mut wire, &body).expect("encode request");
    if prefix_sent {
        stream.write_all(&wire[..4]).expect("send prefix");
        wire.drain(..4);
    }
    let mut writer = stream.try_clone().expect("clone peer");
    let started = Instant::now();
    let closed = std::thread::scope(|scope| {
        let (stop, stopped) = std::sync::mpsc::sync_channel::<()>(1);
        scope.spawn(move || {
            for byte in wire {
                if writer.write_all(&[byte]).is_err() {
                    break;
                }
                if stopped.recv_timeout(interval) != Err(std::sync::mpsc::RecvTimeoutError::Timeout)
                {
                    break;
                }
            }
        });
        let closed = closed_without_reply(&mut stream);
        drop(stop);
        closed
    });
    assert!(closed, "a progressing incomplete frame must expire");
    assert!(
        started.elapsed() >= IO_TIMEOUT - Duration::from_millis(100),
        "short polling waits must not expire the request"
    );
    assert!(started.elapsed() < IO_TIMEOUT + Duration::from_secs(3));
    assert!(
        running.dispatch.calls().is_empty(),
        "incomplete requests never dispatch"
    );
    assert_eq!(call(&running, &tool_request())["ok"], true);
}

#[test]
fn a_trickling_header_cannot_extend_the_request_deadline() {
    assert_trickle_expires(false, IO_TIMEOUT * 2 / 5);
}

#[test]
fn a_trickling_body_cannot_extend_the_request_deadline() {
    assert_trickle_expires(true, IO_TIMEOUT / 20);
}

#[test]
fn header_and_body_share_one_request_deadline() {
    assert_trickle_expires(false, IO_TIMEOUT / 5);
}

#[test]
fn dropping_the_broker_cancels_and_joins_incomplete_frame_readers() {
    let running = start(own_anchor());
    let mut peers: Vec<_> = (0..IN_FLIGHT + QUEUED).map(|_| connect(&running)).collect();
    for (index, peer) in peers.iter_mut().enumerate() {
        if index % 2 == 0 {
            peer.write_all(&MAX_REQUEST_BYTES.to_be_bytes())
                .expect("body pending");
        } else {
            peer.write_all(&[0]).expect("prefix pending");
        }
    }
    // The busy reply proves these authenticated connections were admitted.
    assert_eq!(
        call_when(&running, |r| r["error"]["code"] == BUSY)["error"]["code"],
        BUSY
    );
    let mut writer = peers[0].try_clone().expect("clone trickling peer");
    let started = Instant::now();
    std::thread::scope(|scope| {
        let (stop, stopped) = std::sync::mpsc::sync_channel::<()>(1);
        scope.spawn(move || {
            for _ in 0..200 {
                if writer.write_all(b" ").is_err() {
                    break;
                }
                if stopped.recv_timeout(Duration::from_millis(10))
                    != Err(std::sync::mpsc::RecvTimeoutError::Timeout)
                {
                    break;
                }
            }
        });
        drop(running.broker);
        drop(stop);
    });
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "drop must join readers promptly"
    );
    for mut peer in peers {
        assert!(closed_without_reply(&mut peer));
    }
    assert!(running.dispatch.calls().is_empty());
}
