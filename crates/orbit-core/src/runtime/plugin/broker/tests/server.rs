//! The listener end to end over its real socket. Clients are this test
//! process, anchored by ancestry so the checks run without a sandbox; the
//! sandboxed paths are covered in `sandbox.rs`.

use std::io::{ErrorKind, Read, Write};
use std::os::unix::net::UnixStream;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use orbit_common::process::ancestry::process_start_key;
use orbit_engine::PluginBrokerHandle;
use serde_json::{Value, json};
use tempfile::{TempDir, tempdir};

use super::super::PluginBroker;
use super::super::peer::PeerAnchor;
use super::super::protocol::{
    BUSY, INVALID_REQUEST, MAX_REQUEST_BYTES, NOT_IMPLEMENTED, REQUEST_TOO_LARGE, read_frame,
    write_frame,
};
use super::super::server::{IN_FLIGHT, QUEUED};

const READ_TIMEOUT: Duration = Duration::from_secs(20);

struct Running {
    broker: PluginBroker,
    _root: TempDir,
}

fn start(anchor: PeerAnchor) -> Running {
    let root = tempdir().expect("global root");
    let broker = PluginBroker::start(root.path(), "run-under-test").expect("start broker");
    broker.bind_anchor(anchor);
    Running {
        broker,
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
    json!({"schema_version": 1, "tool": "pulsar.post", "input": {}})
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
fn an_authenticated_request_is_answered_not_implemented() {
    let running = start(own_anchor());

    let response = call(&running, &tool_request());

    assert_eq!(response["ok"], false);
    assert_eq!(response["error"]["code"], NOT_IMPLEMENTED);
    assert_eq!(response["error"]["retryable"], false);
}

#[test]
fn a_malformed_request_is_answered_invalid() {
    let running = start(own_anchor());

    let response = call(&running, br#"{"schema_version": 1}"#);

    assert_eq!(response["error"]["code"], INVALID_REQUEST);
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
    assert_eq!(recovered["error"]["code"], NOT_IMPLEMENTED);
}

/// Call until `accept` holds for the reply. Admission is asynchronous, so the
/// broker may briefly lag the connections this test opened or closed.
fn call_when(running: &Running, accept: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + READ_TIMEOUT;
    loop {
        let response = call(running, &tool_request());
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
