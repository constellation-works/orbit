//! The listener end to end over its real socket. Clients are this test
//! process, anchored by ancestry so the checks run without a sandbox; the
//! sandboxed paths are covered in `sandbox.rs`.

use std::io::{ErrorKind, Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::time::{Duration, Instant};

use orbit_common::process::ancestry::process_start_key;
use orbit_engine::PluginBrokerHandle;
use serde_json::{Value, json};
use tempfile::TempDir;

use super::super::PluginBroker;
use super::super::peer::PeerAnchor;
use super::super::protocol::{MAX_REQUEST_BYTES, read_frame, write_frame};
use super::super::server::IO_TIMEOUT;
use super::EchoDispatch;

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
            ErrorKind::InvalidInput
                | ErrorKind::NotConnected
                | ErrorKind::ConnectionReset
                | ErrorKind::BrokenPipe
        ),
    }
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
