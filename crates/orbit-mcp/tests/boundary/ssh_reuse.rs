//! Exercise both public transports in isolated children with a fake SSH peer.

#![allow(
    clippy::print_stderr,
    reason = "archive fixture timings as diagnostic test evidence"
)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use orbit_mcp::federated::{Destination, DestinationProbe, SshDestinationProbe};
use orbit_mcp::{McpSessionAuthority, RemoteProxyArgs, serve_mcp_remote_proxy};
use serde_json::Value;

const CHILD: &str = "ORBIT_SSH_REUSE_FIXTURE";

// Evaluate the actual builder's argv with OpenSSH, then emulate its control
// socket and an MCP peer. No remote login or persistent process is started.
const STUB: &str = r#"#!/usr/bin/env python3
import json, os, socket, subprocess, sys, time
argv = sys.argv[1:]
# A sandbox may project /etc files with nobody ownership, which OpenSSH
# refuses. Supply a fixture system config rather than depending on host policy.
native_argv = list(argv)
index = native_argv.index('-F') + 1
with open(native_argv[index]) as generated:
    contents = generated.read()
native_config = native_argv[index] + '.fixture'
with open(native_config, 'w') as fixture:
    fixture.write(contents.replace('/etc/ssh/ssh_config', os.environ['ORBIT_SSH_SYSTEM_CONFIG']))
native_argv[index] = native_config
effective = subprocess.check_output(['/usr/bin/ssh', '-G'] + native_argv, text=True)
config = dict(line.split(' ', 1) for line in effective.splitlines())
path = config.get('controlpath')
reuse = path and path != 'none'
cold = not reuse or not os.path.exists(path)
if cold:
    time.sleep(0.2)
    if reuse and config['controlmaster'] != 'false':
        sock = socket.socket(socket.AF_UNIX)
        temporary = path + '.' + 'x' * 16
        sock.bind(temporary)
        sock.close()
        os.link(temporary, path)
        os.unlink(temporary)
with open(os.environ['ORBIT_SSH_JOURNAL'], 'a') as journal:
    journal.write(json.dumps({'argv': argv, 'config': config, 'cold': cold,
                              'cwd': os.getcwd()}) + '\n')
if os.environ['ORBIT_SSH_TRANSPORT'] == 'proxy':
    sys.exit(0)
for line in sys.stdin:
    request = json.loads(line)
    if 'id' not in request:
        continue
    if request['method'] == 'initialize':
        result = {'protocolVersion': request['params']['protocolVersion']}
    elif request['method'] == 'tools/list':
        result = {'tools': []}
    else:
        raise RuntimeError('unexpected request: ' + request['method'])
    print(json.dumps({'jsonrpc': '2.0', 'id': request['id'], 'result': result}), flush=True)
"#;

#[test]
fn ssh_transports_reuse_private_bounded_sockets_and_respect_native_config() {
    if let Ok(case) = std::env::var(CHILD) {
        exercise_child(&case);
        return;
    }
    for transport in ["proxy", "federated"] {
        for case in [
            "defaults",
            "user-no",
            "include-match",
            "long-home",
            "long-home-no",
            "near-limit",
            "over-limit",
            "repair-mode",
            "symlink",
        ] {
            let fixture = tempfile::tempdir().unwrap();
            let bin = fixture.path().join("bin");
            fs::create_dir(&bin).unwrap();
            let stub = bin.join("ssh");
            fs::write(&stub, STUB).unwrap();
            fs::set_permissions(&stub, fs::Permissions::from_mode(0o700)).unwrap();
            let home_name = if case.starts_with("long-home") {
                "a-home-directory-with-spaces-and-percent-%-and-a-name-longer-than-a-unix-socket-path".to_string()
            } else if matches!(case, "near-limit" | "over-limit") {
                // ~/.orbit/ssh/ plus the rendered hash takes 52 bytes. On
                // platforms with an already-long temp root, even a one-byte
                // home name exercises the relative-path fallback instead.
                let desired_home = if case == "near-limit" { 34_usize } else { 35 };
                "h".repeat(
                    desired_home
                        .saturating_sub(fixture.path().as_os_str().as_encoded_bytes().len() + 1)
                        .max(1),
                )
            } else {
                "home".to_string()
            };
            let home = fixture.path().join(home_name);
            fs::create_dir(&home).unwrap();
            let journal = fixture.path().join("journal.jsonl");
            let system_config = fixture.path().join("system.conf");
            fs::write(&system_config, "Host *\n  ServerAliveInterval 23\n").unwrap();
            let output = orbit_common::process::run_bounded(
                Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", "ssh_reuse::ssh_transports_reuse_private_bounded_sockets_and_respect_native_config", "--nocapture"])
                    .current_dir(&home)
                    .env(CHILD, case)
                    .env("HOME", &home)
                    .env("PATH", format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()))
                    .env("ORBIT_SSH_TRANSPORT", transport)
                    .env("ORBIT_SSH_SYSTEM_CONFIG", &system_config)
                    .env("ORBIT_SSH_JOURNAL", &journal),
                Duration::from_secs(30),
            ).unwrap();
            assert!(
                output.status.success(),
                "{transport}/{case}: {} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            // Preserve timing output in nextest's archived passing-test output.
            eprintln!(
                "{transport}/{case}: {} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}

fn exercise_child(case: &str) {
    let home = orbit_common::fs::path::home_dir().unwrap();
    let sockets = home.join(".orbit/ssh");
    fs::create_dir(home.join(".ssh")).unwrap();
    match case {
        "user-no" | "long-home-no" => fs::write(
            home.join(".ssh/config"),
            "Host orbit-fixture\n  ControlMaster no\n  ControlPath none\n",
        )
        .unwrap(),
        "include-match" => {
            // Native relative Include paths are rooted in the user's real
            // ~/.ssh, so use an absolute fixture include instead.
            fs::write(
                home.join(".ssh/config"),
                format!("Include \"{}\"\n", home.join(".ssh/extra.conf").display()),
            )
            .unwrap();
            fs::write(home.join(".ssh/extra.conf"), "Match originalhost orbit-fixture\n  ControlMaster auto\n  ControlPersist 17\n  ControlPath user-%C\n").unwrap();
        }
        "repair-mode" => {
            fs::create_dir(home.join(".orbit")).unwrap();
            fs::create_dir(&sockets).unwrap();
            fs::set_permissions(&sockets, fs::Permissions::from_mode(0o755)).unwrap();
        }
        "symlink" => {
            fs::create_dir(home.join(".orbit")).unwrap();
            let outside = home.join("outside");
            fs::create_dir(&outside).unwrap();
            fs::set_permissions(&outside, fs::Permissions::from_mode(0o755)).unwrap();
            std::os::unix::fs::symlink(&outside, &sockets).unwrap();
            assert!(
                invoke().is_err(),
                "a symlink must not redirect control socket setup"
            );
            assert_eq!(
                fs::metadata(outside).unwrap().permissions().mode() & 0o777,
                0o755
            );
            return;
        }
        _ => {}
    }
    let first = Instant::now();
    invoke().unwrap();
    let first = first.elapsed();
    let second = Instant::now();
    invoke().unwrap();
    let second = second.elapsed();
    eprintln!(
        "cold={}ms second={}ms",
        first.as_millis(),
        second.as_millis()
    );

    assert_eq!(
        fs::metadata(&sockets).unwrap().permissions().mode() & 0o7777,
        0o700,
        "authenticated control sockets must be private, including an existing directory"
    );
    let records = fs::read_to_string(std::env::var_os("ORBIT_SSH_JOURNAL").unwrap())
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["cold"], true);
    assert_eq!(
        records[1]["cold"],
        case.ends_with("no"),
        "the warm call must reuse the control socket unless the user opted out"
    );
    for record in &records {
        let argv = record["argv"].as_array().unwrap();
        assert_eq!(argv[0], "-T");
        assert_eq!(argv[1], "-F");
        assert_eq!(argv[3], "--");
        assert_eq!(argv[4], "orbit-fixture");
        let config = &record["config"];
        assert_eq!(
            config["serveraliveinterval"], "23",
            "system config is evaluated before defaults"
        );
        if case.ends_with("no") {
            assert!(
                config["controlpath"].is_null() || config["controlpath"] == "none",
                "explicit opt-out also disables attaching to an existing master"
            );
            assert_eq!(
                config["controlmaster"], "false",
                "explicit opt-out has precedence"
            );
        } else {
            assert_eq!(config["controlmaster"], "auto");
            if case == "include-match" {
                assert_eq!(
                    config["controlpersist"], "17",
                    "user ControlPersist has precedence"
                );
            } else {
                assert!(
                    config["controlpersist"]
                        .as_str()
                        .and_then(|value| value.parse::<u64>().ok())
                        .is_some_and(|seconds| seconds > 0),
                    "default persistence must keep the connection open after a session ends"
                );
            }
            let path = config["controlpath"].as_str().unwrap();
            assert!(
                path.len() <= 104,
                "rendered socket path is {} bytes: {path}",
                path.len()
            );
            if Path::new(path).is_relative() && case != "include-match" {
                assert_eq!(record["cwd"].as_str().unwrap(), sockets.to_str().unwrap());
                assert_eq!(
                    path.len(),
                    40,
                    "long state paths use only the connection hash"
                );
            } else if case != "include-match" {
                assert_eq!(Path::new(path).parent(), Some(sockets.as_path()));
            } else {
                assert!(path.starts_with("user-"), "user ControlPath has precedence");
            }
            if case != "include-match" {
                assert!(
                    path.len() + 17 < 104,
                    "default must also fit OpenSSH's temporary socket suffix and NUL"
                );
            }
        }
    }
}

fn invoke() -> Result<(), orbit_common::OrbitError> {
    if std::env::var("ORBIT_SSH_TRANSPORT").unwrap() == "proxy" {
        serve_mcp_remote_proxy(RemoteProxyArgs {
            ssh_host: "orbit-fixture".to_string(),
            orchestrator: None,
            authority: McpSessionAuthority::Agent,
        })
    } else {
        let probe = SshDestinationProbe::new(
            "fixture".to_string(),
            Duration::from_secs(5),
            Duration::from_secs(5),
            None,
            McpSessionAuthority::Agent,
        );
        let mut route = probe.open_route(&Destination::ssh("orbit-fixture", "hm_fixture"))?;
        assert!(route.advertised_tools()?.is_empty());
        Ok(())
    }
}
