use std::io::ErrorKind;
use std::os::unix::net::UnixListener;

use orbit_common::process::ancestry::process_start_key;
use orbit_engine::PluginBrokerHandle;
use serde_json::json;

use super::super::PluginBroker;
use super::super::client::{forward_call, require_same_uid};
use super::super::peer::PeerAnchor;
use super::super::protocol::{error_response, write_frame};
use super::{EchoDispatch, REFUSED_TOOL};

#[test]
fn forwards_a_call_and_preserves_broker_output_and_errors() {
    let root = super::short_tempdir();
    let dispatch = EchoDispatch::shared();
    let broker =
        PluginBroker::start(root.path(), "client-test", dispatch.clone()).expect("start broker");
    broker.bind_anchor(PeerAnchor::Ancestor(
        process_start_key(std::process::id()).expect("own start key"),
    ));
    let output = forward_call(
        broker.socket_path(),
        "fixture.echo",
        json!({"text": "hi"}),
        root.path(),
        Some("workspace"),
        "mcp",
    )
    .expect("forwarded output");
    assert_eq!(output["tool"], "fixture.echo");
    let calls = dispatch.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0.input, json!({"text": "hi"}));
    assert_eq!(calls[0].0.workspace.as_deref(), Some("workspace"));
    assert_eq!(calls[0].0.entry_point, super::super::EntryPoint::Mcp);
    assert!(!calls[0].0.dry_run);

    let error = forward_call(
        broker.socket_path(),
        REFUSED_TOOL,
        json!({}),
        root.path(),
        None,
        "cli",
    )
    .expect_err("broker refusal");
    let orbit_common::OrbitError::RemoteTool { code, payload, .. } = error else {
        panic!("broker refusal must retain a structured code");
    };
    assert_eq!(code, "plugin_broker_refused");
    assert_eq!(payload["retryable"], false);
}

#[test]
fn unavailable_and_busy_keep_distinct_retryability() {
    let root = super::short_tempdir();
    let missing = root.path().join("missing.sock");
    let error = forward_call(
        &missing,
        "fixture.echo",
        json!({}),
        root.path(),
        None,
        "cli",
    )
    .expect_err("missing broker");
    let orbit_common::OrbitError::RemoteTool { payload, .. } = error else {
        panic!("unavailable must be structured");
    };
    assert_eq!(payload["code"], "plugin_broker_unavailable");
    assert_eq!(payload["retryable"], false);

    let socket = root.path().join("busy.sock");
    let listener = UnixListener::bind(&socket).expect("bind fake busy broker");
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept client");
        let body = error_response("plugin_broker_busy", "queue full", true);
        write_frame(&mut stream, &body).expect("answer busy");
    });
    let error = forward_call(&socket, "fixture.echo", json!({}), root.path(), None, "cli")
        .expect_err("busy broker");
    server.join().expect("server thread");
    let orbit_common::OrbitError::RemoteTool { payload, .. } = error else {
        panic!("busy must be structured");
    };
    assert_eq!(payload["code"], "plugin_broker_busy");
    assert_eq!(payload["retryable"], true);
}

#[test]
fn a_different_server_uid_is_refused() {
    assert!(require_same_uid(1000, 1000).is_ok());
    assert_eq!(
        require_same_uid(1001, 1000)
            .expect_err("foreign uid")
            .kind(),
        ErrorKind::PermissionDenied
    );
}
