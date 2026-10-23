//! Framing diagnostics through the exported SSH transport, with no live host.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use std::time::{Duration, Instant};

use orbit_common::OrbitError;
use orbit_mcp::McpSessionAuthority;
use orbit_mcp::federated::{Destination, DestinationProbe, SshDestinationProbe};
use orbit_types::tool::ToolSessionContext;
use serde_json::{Value, json};

const CHILD: &str = "ORBIT_OVERSIZED_LINE_FIXTURE";
const CAP: u64 = 1024;
const PAYLOAD_SECRET: &str = "PAYLOAD_SECRET_MUST_NOT_ESCAPE";
const ARGUMENT_SECRET: &str = "ARGUMENT_SECRET_MUST_NOT_ESCAPE";
const STDERR_SECRET: &str = "STDERR_SECRET_MUST_NOT_ESCAPE";

const PEER: &str = r#"#!/usr/bin/env python3
import json, os, sys
case = os.environ['ORBIT_OVERSIZED_LINE_FIXTURE']
with open('peer.pid', 'w') as file:
    file.write(str(os.getpid()))
for line in sys.stdin:
    request = json.loads(line)
    if 'id' not in request:
        continue
    with open('journal', 'a') as file:
        file.write(request['method'] + '\n')
    if request['method'] == 'initialize' and case != 'initialize':
        result = {'protocolVersion': request['params']['protocolVersion']}
        if case != 'peer-request':
            result['serverInfo'] = {'name': 'fixture', 'version': '1.2.3'}
    elif case == 'normal':
        result = {'structuredContent': {'ok': True}}
    else:
        print('STDERR_SECRET_MUST_NOT_ESCAPE', file=sys.stderr, flush=True)
        if case in ('notification', 'bad-projection'):
            prefix = '{"jsonrpc":"2.0","method":"notifications/message","params":"'
        elif case == 'peer-request':
            prefix = '{"jsonrpc":"2.0","id":%s,"method":"ping","params":"' % request['id']
        else:
            prefix = 'unparseable:'
        # A finite oversized prefix without a newline. Wait for EOF rather
        # than finishing it: diagnostics must not drain to compute a length.
        suffix = '"}\n' if case in ('notification', 'bad-projection', 'peer-request') else ''
        sys.stdout.write(prefix + 'PAYLOAD_SECRET_MUST_NOT_ESCAPE' + 'x' * 1100 + suffix)
        sys.stdout.flush()
        for remaining in sys.stdin:
            with open('journal', 'a') as file:
                file.write('unexpected-replay\n')
        sys.exit(0)
    print(json.dumps({'jsonrpc': '2.0', 'id': request['id'], 'result': result}), flush=True)
"#;

#[test]
fn oversized_lines_retain_safe_uncorrelated_context_without_replay() {
    if let Ok(case) = std::env::var(CHILD) {
        exercise(&case);
        return;
    }
    for case in [
        "notification",
        "peer-request",
        "unparseable",
        "bad-projection",
        "initialize",
        "normal",
    ] {
        let fixture = tempfile::tempdir().unwrap();
        let bin = fixture.path().join("bin");
        fs::create_dir(&bin).unwrap();
        let peer = bin.join("ssh");
        fs::write(&peer, PEER).unwrap();
        fs::set_permissions(&peer, fs::Permissions::from_mode(0o700)).unwrap();
        let output = orbit_common::process::run_bounded(
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "oversized_lines::oversized_lines_retain_safe_uncorrelated_context_without_replay",
                    "--nocapture",
                ])
                .current_dir(fixture.path())
                .env(CHILD, case)
                .env("HOME", fixture.path())
                .env(
                    "PATH",
                    format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
                ),
            Duration::from_secs(15),
        )
        .unwrap();
        let captured = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        for secret in [PAYLOAD_SECRET, ARGUMENT_SECRET, STDERR_SECRET] {
            assert!(
                !captured.contains(secret),
                "{case}: diagnostic logs leaked a secret"
            );
        }
        assert!(output.status.success(), "{case}: {captured}");
        let journal = fs::read_to_string(fixture.path().join("journal")).unwrap();
        assert_eq!(
            journal,
            if case == "initialize" {
                "initialize\n"
            } else {
                "initialize\ntools/call\n"
            },
            "the transport must not replay the dispatched call or add a version probe"
        );
        let pid: i32 = fs::read_to_string(fixture.path().join("peer.pid"))
            .unwrap()
            .parse()
            .unwrap();
        // SAFETY: signal zero observes existence without signaling a process.
        assert_eq!(
            unsafe { libc::kill(pid, 0) },
            -1,
            "the SSH peer must be reaped"
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }
}

fn exercise(case: &str) {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .init();
    let started = Instant::now();
    let probe = SshDestinationProbe::new(
        "hm_caller".into(),
        Duration::from_secs(5),
        Duration::from_secs(5),
        None,
        McpSessionAuthority::Agent,
    )
    .with_test_line_cap(CAP);
    let route = probe.open_route(&Destination::ssh("fake-peer", "hm_fixture"));
    let error = if case == "initialize" {
        match route {
            Ok(_) => panic!("oversized initialization must be refused"),
            Err(error) => error,
        }
    } else {
        let mut route = route.unwrap();
        let result = route.call_tool(
            "orbit.task.show",
            json!({
                "id": ARGUMENT_SECRET,
                "_worker_read": if case == "bad-projection" { ARGUMENT_SECRET } else { "task" },
                "fields": ["title", ARGUMENT_SECRET, "status"],
                ARGUMENT_SECRET: ARGUMENT_SECRET,
            }),
            ToolSessionContext::default(),
        );
        if case == "normal" {
            assert_eq!(result.unwrap(), json!({"ok": true}));
            return;
        }
        result.unwrap_err()
    };
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "refusal and cleanup must not await the unterminated line or the deadline"
    );
    let rendered = error.to_string();
    for secret in [PAYLOAD_SECRET, ARGUMENT_SECRET, STDERR_SECRET] {
        assert!(
            !rendered.contains(secret),
            "returned diagnostics leaked a secret"
        );
    }
    assert!(rendered.len() < 2048, "diagnostics must remain bounded");
    let message = match error {
        OrbitError::OutcomeUnknown {
            mcp_call_id,
            message,
        } if case != "initialize" => {
            assert_eq!(mcp_call_id, "hm_fixture/orbit.task.show#2");
            message
        }
        OrbitError::UnreachableDestination(message) if case == "initialize" => message,
        other => panic!("incorrect dispatch classification: {other}"),
    };
    let (_, metadata) = message.split_once("diagnostic=").unwrap_or_else(|| {
        panic!("oversized framing must retain safe diagnostic metadata: {message}")
    });
    let diagnostic = serde_json::Deserializer::from_str(metadata)
        .into_iter::<Value>()
        .next()
        .unwrap()
        .unwrap();
    assert_eq!(diagnostic["correlation"], "unknown");
    assert_eq!(diagnostic["destination_machine_id"], "hm_fixture");
    assert_eq!(diagnostic["local_version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(diagnostic["local_build"], "unknown");
    assert_eq!(diagnostic["peer_build"], "unknown");
    assert_eq!(
        diagnostic["peer_version"],
        if matches!(case, "initialize" | "peer-request") {
            "unknown"
        } else {
            "1.2.3"
        }
    );
    assert_eq!(diagnostic["line_cap_bytes"], CAP);
    let observed = diagnostic["observed_bytes_lower_bound"].as_u64().unwrap();
    assert!(
        observed > CAP && observed <= 1300,
        "count only observed bytes of the refused line"
    );
    let pending = &diagnostic["pending"];
    assert_eq!(
        pending["request_id"],
        if case == "initialize" { 1 } else { 2 }
    );
    assert_eq!(
        pending["phase"],
        if case == "initialize" {
            "probe"
        } else {
            "dispatched_call"
        }
    );
    assert_eq!(
        pending["method"],
        if case == "initialize" {
            "initialize"
        } else {
            "tools/call"
        }
    );
    assert_eq!(
        pending["tool"],
        if case == "initialize" {
            "unknown"
        } else {
            "orbit_task_show"
        }
    );
    assert_eq!(
        pending["projection"]["_worker_read"],
        if matches!(case, "initialize" | "bad-projection") {
            "unknown"
        } else {
            "task"
        }
    );
    assert_eq!(
        pending["projection"]["selected_fields"],
        if case == "initialize" {
            json!([])
        } else {
            json!(["title", "status"])
        }
    );
}
