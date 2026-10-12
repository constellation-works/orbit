//! `/api/on/:host/*rest` between two real dashboards. A serves; B is a second
//! fixture with its own root and machine identity; A's `ssh` is a stub with
//! OpenSSH's `-L` argv contract whose forward lands on B's real port.
//! Tunnel lifecycle (single flight, dead child, idle) is in the unit tests.

use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, TcpListener};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use orbit_common::test_fixtures::TEST_CODEX_MODEL;
use orbit_core::AuditEventStatus;
use orbit_core::JobRunState;
use orbit_core::TaskStatus;
use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
use orbit_types::task::TaskArtifact;
use reqwest::blocking::Response;
use serde_json::{Value, json};

use super::audit::row as audit_row;
use super::support::{Fixture, Server, error_code, isolated, json_ok, walk_audit_pages};

const WS: &str = "?workspace=ws_http_fixture";
/// A spawn first waits out the attach budget, longer than the default client timeout.
const SLOW_REQUEST: Duration = Duration::from_secs(40);

/// The stub's directory, config and call log. Dropping it kills every stub it
/// logged, so a failed assertion cannot leak one.
struct StubSsh {
    dir: tempfile::TempDir,
}

impl StubSsh {
    fn new(hosts: Value) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("ssh");
        fs::write(&script, include_str!("stub_ssh.py")).unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let config = json!({
            "log": dir.path().join("calls.jsonl"),
            "release": dir.path().join("release"),
            "hosts": hosts,
        });
        fs::write(dir.path().join("ssh.json"), config.to_string()).unwrap();
        Self { dir }
    }

    fn bin(&self) -> &Path {
        self.dir.path()
    }

    /// Lets every held exit host's ssh run on to its exit.
    fn release(&self) {
        fs::write(self.dir.path().join("release"), "").unwrap();
    }

    fn calls(&self) -> Vec<Value> {
        fs::read_to_string(self.dir.path().join("calls.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn pids(&self) -> Vec<i64> {
        self.calls()
            .iter()
            .map(|call| call["pid"].as_i64().unwrap())
            .collect()
    }
}

impl Drop for StubSsh {
    fn drop(&mut self) {
        for pid in self.pids() {
            // SAFETY: signalling a pid this test's stub recorded.
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGKILL);
            }
        }
    }
}

/// `kill(pid, 0)`: true while the process exists, including as a zombie.
fn alive(pid: i64) -> bool {
    // SAFETY: signal 0 only checks existence.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

fn wait_until(what: &str, timeout: Duration, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(20));
    }
}

fn port(server: &Server) -> u16 {
    server.origin.rsplit(':').next().unwrap().parse().unwrap()
}

/// Write `hosts.toml` schema v1 with `(name, machine_id, ssh, task_prefix)` rows.
fn write_hosts(fixture: &Fixture, entries: &[(&str, &str, &str, &str)]) {
    let mut text = String::from("schema_version = 1\n");
    for (name, machine_id, ssh, prefix) in entries {
        text.push_str(&format!(
            "\n[[hosts]]\nname = \"{name}\"\nmachine_id = \"{machine_id}\"\nssh = \"{ssh}\"\ntask_prefix = \"{prefix}\"\n"
        ));
    }
    fs::write(fixture.global.join("hosts.toml"), text).unwrap();
}

/// A dashboard from before `/api/hosts`: healthy, and 404 for everything else.
fn old_dashboard() -> u16 {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    serve_dashboard(listener, None);
    port
}

/// Answer `/healthz` with 200, `/api/hosts?probe=false` with `identity` (404
/// without one) and everything else with 404. Returns the connection count.
fn serve_dashboard(listener: TcpListener, identity: Option<Value>) -> Arc<AtomicUsize> {
    let connections = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&connections);
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            counted.fetch_add(1, Ordering::SeqCst);
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            let mut buf = [0u8; 2048];
            let n = stream.read(&mut buf).unwrap_or(0);
            let request = &buf[..n];
            let (status, body) = match &identity {
                _ if request.starts_with(b"GET /healthz ") => ("200 OK", String::new()),
                Some(identity) if request.starts_with(b"GET /api/hosts?probe=false ") => {
                    ("200 OK", identity.to_string())
                }
                _ => ("404 Not Found", String::new()),
            };
            let _ = write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    connections
}

/// The local port a stub call was told to forward (`-L 127.0.0.1:<port>:…`).
fn forwarded_port(call: &Value) -> u16 {
    let argv: Vec<&str> = call["argv"]
        .as_array()
        .unwrap()
        .iter()
        .map(|arg| arg.as_str().unwrap())
        .collect();
    let spec = argv[argv.iter().position(|arg| *arg == "-L").unwrap() + 1];
    spec.split(':').nth(1).unwrap().parse().unwrap()
}

/// B's task with an HTML artifact, which the dashboard serves as a sandboxed download.
fn seed_task(fixture: &Fixture) -> String {
    let task = fixture
        .runtime
        .add_task(TaskAddParams {
            title: "Remote task".into(),
            description: "Lives on B.".into(),
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .unwrap();
    fixture
        .runtime
        .update_task_with_identity(
            &task.id,
            TaskUpdateParams {
                upsert_artifacts: vec![TaskArtifact {
                    path: "reports/page.html".into(),
                    media_type: "text/html; charset=utf-8".into(),
                    content: b"<p>remote artifact</p>".to_vec(),
                    created_by: None,
                }],
                ..Default::default()
            },
            Some("codex".into()),
            Some(TEST_CODEX_MODEL.into()),
        )
        .unwrap();
    task.id
}

fn log_line(step: &str) -> String {
    format!(
        "{}\n",
        json!({
            "timestamp":"2026-10-08T01:00:00Z", "level":"INFO", "target":"orbit.job.step_started",
            "fields":{"job_run_id":"host-forward","step_id":step},
        })
    )
}

/// Read SSE events until one carries `step`; the request timeout bounds it.
fn await_event(reader: &mut BufReader<Response>, step: &str) {
    loop {
        let mut line = String::new();
        assert!(
            reader
                .read_line(&mut line)
                .expect("SSE line within timeout")
                > 0,
            "SSE stream ended before {step}"
        );
        if line.starts_with("data:") && line.contains(step) {
            return;
        }
    }
}

fn comments(server: &Server, id: &str) -> usize {
    json_ok(server.get(&format!("/api/tasks/{id}{WS}")))["comments"]
        .as_array()
        .unwrap()
        .len()
}

/// Browser fetch must decode the gzip that crosses both dashboard servers.
#[test]
#[ignore = "requires ORBIT_PLAYWRIGHT_MODULE and ORBIT_COMPRESSION_BROWSER_EVIDENCE_DIR"]
fn gzip_forward_returns_json_to_the_browser() {
    isolated(
        "host_forward::gzip_forward_returns_json_to_the_browser",
        || {
            let b = Fixture::host("hostb", "HB");
            b.seed_run("jrun-browser-compression", "fixture", JobRunState::Success);
            let b_server = b.server(false);
            let a = Fixture::new();
            write_hosts(&a, &[("hostb", &b.machine_id(), "hostb-ssh", "HB")]);
            let stub =
                StubSsh::new(json!({"hostb-ssh": {"attach": port(&b_server), "spawn": null}}));
            let a_server = a.server_with_path(false, stub.bin());
            let output = std::process::Command::new("node")
                .arg(
                    Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join("tests/http_api/compression_browser.mjs"),
                )
                .arg(
                    std::env::var_os("ORBIT_PLAYWRIGHT_MODULE")
                        .expect("prepared Playwright module"),
                )
                .arg(
                    std::env::var_os("ORBIT_COMPRESSION_BROWSER_EVIDENCE_DIR")
                        .expect("browser evidence directory"),
                )
                .arg(&a_server.origin)
                .output()
                .expect("launch compression browser fixture");
            assert!(
                output.status.success(),
                "browser gzip forwarding failed:\n{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        },
    );
}

#[test]
fn gzip_forward_negotiates_upstream_and_preserves_encoding_and_vary() {
    isolated(
        "host_forward::gzip_forward_negotiates_upstream_and_preserves_encoding_and_vary",
        || {
            let b = Fixture::host("hostb", "HB");
            let b_server = b.server(false);
            let identity = json_ok(b_server.get("/api/hosts?probe=false")).to_string();
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            let remote_port = listener.local_addr().unwrap().port();
            // A remote stub exposes the received negotiation in its JSON.
            // One route is pre-encoded; the other models an older dashboard.
            thread::spawn(move || {
                for stream in listener.incoming() {
                    let mut stream = stream.unwrap();
                    stream
                        .set_read_timeout(Some(Duration::from_secs(10)))
                        .unwrap();
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut first = String::new();
                    reader.read_line(&mut first).unwrap();
                    let mut encoding = String::new();
                    loop {
                        let mut line = String::new();
                        reader.read_line(&mut line).unwrap();
                        if line == "\r\n" || line.is_empty() {
                            break;
                        }
                        if let Some((name, value)) = line.split_once(':')
                            && name.eq_ignore_ascii_case("accept-encoding")
                        {
                            encoding = value.trim().to_string();
                        }
                    }
                    let mut extra_headers = String::new();
                    let body = if first.starts_with("GET /healthz ") {
                        Vec::new()
                    } else if first.starts_with("GET /api/hosts?") {
                        identity.as_bytes().to_vec()
                    } else {
                        let json = serde_json::to_vec(&json!({
                            "accept_encoding": encoding,
                            "message": "remote JSON payload".repeat(100),
                        }))
                        .unwrap();
                        extra_headers.push_str("Vary: Accept\r\nVary: Accept-Encoding\r\n");
                        if first.starts_with("GET /api/encoded?") {
                            extra_headers.push_str("Content-Encoding: gzip\r\n");
                            let mut gzip = flate2::write::GzEncoder::new(
                                Vec::new(),
                                flate2::Compression::default(),
                            );
                            gzip.write_all(&json).unwrap();
                            gzip.finish().unwrap()
                        } else {
                            json
                        }
                    };
                    write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json; charset=utf-8\r\n{extra_headers}Content-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
                    stream.write_all(&body).unwrap();
                }
            });
            let a = Fixture::new();
            write_hosts(&a, &[("hostb", &b.machine_id(), "hostb-ssh", "HB")]);
            let stub = StubSsh::new(json!({"hostb-ssh": {"attach": remote_port, "spawn": null}}));
            let a_server = a.server_with_path(false, stub.bin());
            for route in ["encoded", "legacy"] {
                let response = a_server
                    .request("GET", &format!("/api/on/hostb/{route}{WS}"))
                    .header("accept-encoding", "gzip")
                    .send()
                    .unwrap();
                assert_eq!(response.status(), 200);
                assert_eq!(response.headers()["content-encoding"], "gzip");
                assert!(response.headers().get("content-length").is_none());
                let vary: Vec<_> = response
                    .headers()
                    .get_all("vary")
                    .iter()
                    .map(|value| value.to_str().unwrap())
                    .collect();
                assert_eq!(vary, ["Accept", "Accept-Encoding"]);
                let bytes = response.bytes().unwrap();
                let mut decoded = Vec::new();
                flate2::read::GzDecoder::new(bytes.as_ref())
                    .read_to_end(&mut decoded)
                    .unwrap();
                let value: Value =
                    serde_json::from_slice(&decoded).expect("one gzip decoding yields JSON");
                assert_eq!(
                    value["accept_encoding"], "gzip",
                    "negotiation crosses the SSH hop"
                );
                assert_eq!(value["message"], "remote JSON payload".repeat(100));
            }
            let response = a_server.get(&format!("/api/on/hostb/legacy{WS}"));
            assert!(response.headers().get("content-encoding").is_none());
            assert_eq!(json_ok(response)["accept_encoding"], "");
        },
    );
}

#[test]
fn reads_through_the_forward_match_the_remote_dashboard() {
    isolated(
        "host_forward::reads_through_the_forward_match_the_remote_dashboard",
        || {
            let b = Fixture::host("hostb", "HB");
            let task = seed_task(&b);
            b.job("remote-job");
            let run = b.seed_run("jrun-remote", "remote-job", JobRunState::Success);
            let b_server = b.server(false);
            let a = Fixture::new();
            write_hosts(&a, &[("hostb", &b.machine_id(), "hostb-ssh", "HB")]);
            let stub =
                StubSsh::new(json!({"hostb-ssh": {"attach": port(&b_server), "spawn": null}}));
            let a_server = a.server_with_path(false, stub.bin());

            for path in [
                "/workspaces".to_string(),
                format!("/tasks{WS}"),
                format!("/tasks/{task}{WS}"),
                format!("/runs/{}{WS}", run.run_id),
            ] {
                assert_eq!(
                    json_ok(a_server.get(&format!("/api/on/hostb{path}"))),
                    json_ok(b_server.get(&format!("/api{path}"))),
                    "{path} through A matches B direct"
                );
            }
            // The resource sample is live, so compare what does not move.
            let forwarded = json_ok(a_server.get("/api/on/HOSTB/host/resources"));
            let direct = json_ok(b_server.get("/api/host/resources"));
            assert_eq!(forwarded["thresholds"], direct["thresholds"]);
            assert_eq!(forwarded["disk"]["path"], direct["disk"]["path"]);

            let artifact = format!("/tasks/{task}/artifacts/reports/page.html{WS}");
            let forwarded = a_server.get(&format!("/api/on/{}{artifact}", b.machine_id()));
            let direct = b_server.get(&format!("/api{artifact}"));
            assert_eq!(forwarded.status(), 200);
            for name in [
                "content-type",
                "content-disposition",
                "content-security-policy",
            ] {
                assert_eq!(
                    forwarded.headers().get(name),
                    direct.headers().get(name),
                    "artifact {name} survives the forward"
                );
            }
            assert_eq!(
                forwarded.headers()["content-security-policy"],
                "sandbox; default-src 'none'"
            );
            assert_eq!(forwarded.bytes().unwrap(), direct.bytes().unwrap());

            let stream = a_server
                .request("GET", "/api/on/hostb/log/stream?from=0")
                .header("accept-encoding", "gzip")
                .timeout(Duration::from_secs(20))
                .send()
                .unwrap();
            assert_eq!(stream.status(), 200);
            assert_eq!(stream.headers()["content-type"], "text/event-stream");
            assert!(stream.headers().get("content-encoding").is_none());
            let mut stream = BufReader::new(stream);
            let mut log = OpenOptions::new()
                .create(true)
                .append(true)
                .open(b.path("process.log"))
                .unwrap();
            log.write_all(log_line("appended-on-b").as_bytes()).unwrap();
            await_event(&mut stream, "appended-on-b");
            drop(stream);

            let local = json_ok(a_server.get(&format!("/api/tasks{WS}")));
            assert_ne!(
                local,
                json_ok(b_server.get(&format!("/api/tasks{WS}"))),
                "A's own /api still answers from A"
            );
            assert_eq!(
                json_ok(a_server.get(&format!("/api/on/http-fixture/tasks{WS}"))),
                local,
                "the serving host's own name is answered locally"
            );

            let state = json_ok(a_server.get("/api/hosts/hostb/connection"));
            assert_eq!(state["reachable"], true, "{state}");
            assert_eq!(state["origin"], "attached");
            assert_eq!(state["machine_id"], b.machine_id().as_str());
            assert_eq!(state["skew"], false, "{state}");
            assert!(state["binary_version"].as_str().is_some(), "{state}");

            let calls = stub.calls();
            assert_eq!(calls.len(), 1, "every read shares one tunnel: {calls:?}");
            assert_eq!(calls[0]["mode"], "attach");
            assert_eq!(
                calls[0]["command"], "",
                "attach mode sends no remote command"
            );
        },
    );
}

#[test]
fn fetch_metadata_blocks_cross_site_gets_before_host_side_effects() {
    isolated(
        "host_forward::fetch_metadata_blocks_cross_site_gets_before_host_side_effects",
        || {
            let b = Fixture::host("hostb", "HB");
            let b_server = b.server(false);
            let a = Fixture::new();
            write_hosts(&a, &[("hostb", &b.machine_id(), "hostb-ssh", "HB")]);
            let stub =
                StubSsh::new(json!({"hostb-ssh": {"attach": port(&b_server), "spawn": null}}));
            let server = a.server_with_path(false, stub.bin());

            for path in [format!("/api/on/hostb/tasks{WS}"), "/api/hosts".to_string()] {
                let response = server
                    .request("GET", &path)
                    .header("sec-fetch-site", "cross-site")
                    .send()
                    .unwrap();
                assert_eq!(response.status(), 403, "cross-site GET {path}");
                assert!(stub.calls().is_empty(), "{path} starts no SSH probe");
            }

            let dashboard = server
                .request("GET", &format!("/api/on/hostb/tasks{WS}"))
                .header("origin", &server.origin)
                .header("sec-fetch-site", "same-origin")
                .send()
                .unwrap();
            assert_eq!(dashboard.status(), 200);
            let _: Value = dashboard.json().unwrap();
            assert_eq!(stub.calls().len(), 1, "same-origin dashboard GET works");

            let cli = json_ok(server.get("/api/hosts?probe=false"));
            assert!(
                cli["hosts"].is_array(),
                "CLI GET without Fetch Metadata works"
            );
        },
    );
}

#[test]
fn forwarded_writes_need_the_operator_session() {
    isolated(
        "host_forward::forwarded_writes_need_the_operator_session",
        || {
            let b = Fixture::host("hostb", "HB");
            let task = seed_task(&b);
            let b_server = b.server(false);
            let a = Fixture::new();
            write_hosts(&a, &[("hostb", &b.machine_id(), "hostb-ssh", "HB")]);
            let stub =
                StubSsh::new(json!({"hostb-ssh": {"attach": port(&b_server), "spawn": null}}));
            let comment = format!("/api/on/hostb/tasks/{task}/comments{WS}");
            let body = json!({"message": "from A"});

            let agent = a.server_with_path(false, stub.bin());
            for (origin, label) in [
                (Some("http://evil.example"), "cross-origin"),
                (None, "no Origin"),
            ] {
                let mut request = agent.request("POST", &comment).json(&body);
                if let Some(origin) = origin {
                    request = request.header("origin", origin);
                }
                assert_eq!(request.send().unwrap().status(), 403, "{label}");
            }
            assert!(stub.calls().is_empty(), "the origin guard runs before ssh");
            let denied = error_code(
                agent.send("POST", &comment, body.clone()),
                403,
                "authorization_denied",
            );
            assert_eq!(denied["operation"], "host.forward", "{denied}");
            assert!(stub.calls().is_empty(), "a denied write starts no ssh");
            assert_eq!(comments(&b_server, &task), 0, "B records nothing");
            json_ok(agent.get(&format!("/api/on/hostb/tasks{WS}")));
            let refused = json_ok(agent.get("/api/hosts/hostb/connection"));
            assert_eq!(refused["forward_writes"]["authorized"], false, "{refused}");
            assert!(
                refused["forward_writes"]["reason"]
                    .as_str()
                    .is_some_and(|reason| !reason.is_empty()),
                "a refused session is told why: {refused}"
            );
            drop(agent);

            let operator = a.server_with_path(true, stub.bin());
            let allowed = json_ok(operator.get("/api/hosts/hostb/connection"));
            assert_eq!(allowed["forward_writes"]["authorized"], true, "{allowed}");
            json_ok(operator.send("POST", &comment, body));
            assert_eq!(comments(&b_server, &task), 1, "the comment lands on B");
        },
    );
}

#[test]
fn forward_failures_are_typed_and_bounded() {
    isolated(
        "host_forward::forward_failures_are_typed_and_bounded",
        || {
            let b = Fixture::host("hostb", "HB");
            let b_server = b.server(false);
            let old = old_dashboard();
            let a = Fixture::new();
            write_hosts(
                &a,
                &[
                    ("hostb", &b.machine_id(), "hostb-ssh", "HB"),
                    ("deadhost", "hm_deadhost", "dead-ssh", "HD"),
                    ("imposter", "hm_imposter", "imposter-ssh", "HI"),
                    ("oldhost", "hm_oldhost", "old-ssh", "HO"),
                ],
            );
            let stub = StubSsh::new(json!({
                "hostb-ssh": {"attach": port(&b_server), "spawn": null},
                "dead-ssh": {"exit": 255, "hold": true},
                "imposter-ssh": {"attach": port(&b_server), "spawn": null},
                "old-ssh": {"attach": old, "spawn": null},
            }));
            let a_server = a.server_with_path(false, stub.bin());

            let unknown = error_code(
                a_server.get(&format!("/api/on/nohost/tasks{WS}")),
                404,
                "unknown_host",
            );
            assert_eq!(unknown["host"], "nohost");
            assert!(stub.calls().is_empty(), "an unknown host starts no ssh");

            for path in [
                "/api/on/hostb/hosts",
                "/api/on/hostb/%68osts",
                "/api/on/hostb/on/hostc/tasks",
            ] {
                error_code(a_server.get(path), 400, "invalid_input");
            }
            assert!(stub.calls().is_empty(), "a refused path starts no ssh");

            let started = Instant::now();
            let origin = a_server.origin.clone();
            let dead = thread::spawn(move || {
                reqwest::blocking::Client::builder()
                    .no_proxy()
                    .timeout(SLOW_REQUEST)
                    .build()
                    .unwrap()
                    .get(format!("{origin}/api/on/deadhost/tasks"))
                    .send()
                    .unwrap()
            });
            wait_until(
                "the dead host's ssh to start",
                Duration::from_secs(10),
                || !stub.calls().is_empty(),
            );
            // The dead host's ssh is held, so its request is still in flight here.
            json_ok(a_server.get(&format!("/api/tasks{WS}")));
            assert!(
                !dead.is_finished(),
                "the dead host's request was still in flight when the local request succeeded"
            );
            stub.release();
            let dead = error_code(dead.join().unwrap(), 502, "unreachable_destination");
            assert!(started.elapsed() < Duration::from_secs(20), "bounded");
            assert!(
                dead["error"].as_str().unwrap().contains("exit 255"),
                "{dead}"
            );
            assert_eq!(dead["host"], "deadhost");
            let state = json_ok(
                a_server
                    .request("GET", "/api/hosts/deadhost/connection")
                    .timeout(SLOW_REQUEST)
                    .send()
                    .unwrap(),
            );
            assert_eq!(state["reachable"], false, "{state}");
            assert_eq!(state["error"]["code"], "unreachable_destination", "{state}");

            let before = stub.calls().len();
            let mismatch = error_code(
                a_server.get("/api/on/imposter/workspaces"),
                409,
                "host_identity_mismatch",
            );
            assert_eq!(mismatch["host"], "imposter");
            let imposter = stub.pids()[before];
            wait_until(
                "the mismatched tunnel's ssh to exit",
                Duration::from_secs(10),
                || !alive(imposter),
            );

            error_code(
                a_server.get("/api/on/oldhost/workspaces"),
                409,
                "host_too_old",
            );
            json_ok(a_server.get("/api/on/hostb/workspaces"));
        },
    );
}

/// The reserved local port is released before ssh binds it. A healthy
/// dashboard that takes it first, reporting another machine, must never be
/// read as the tunnel: the request fails with ssh's own exit, not a 409.
#[test]
fn a_foreign_listener_on_the_reserved_port_is_never_the_tunnel() {
    isolated(
        "host_forward::a_foreign_listener_on_the_reserved_port_is_never_the_tunnel",
        || {
            let a = Fixture::new();
            write_hosts(
                &a,
                &[
                    ("deadhost", "hm_deadhost", "dead-ssh", "HD"),
                    ("takenhost", "hm_takenhost", "taken-ssh", "HT"),
                ],
            );
            let behind = old_dashboard();
            // `dead-ssh` never binds. `taken-ssh` authenticates for
            // `bind_delay`, then finds the port taken and exits at once.
            let stub = StubSsh::new(json!({
                "dead-ssh": {"exit": 255, "delay": 1.5},
                "taken-ssh": {"attach": behind, "spawn": behind, "bind_delay": 1.5},
            }));
            let a_server = a.server_with_path(false, stub.bin());
            let foreign_identity = json!({"hosts": [{"local": true, "machine_id": "hm_foreign"}]});

            for host in ["deadhost", "takenhost"] {
                let before = stub.calls().len();
                let origin = a_server.origin.clone();
                let request = thread::spawn(move || {
                    reqwest::blocking::Client::builder()
                        .no_proxy()
                        .timeout(SLOW_REQUEST)
                        .build()
                        .unwrap()
                        .get(format!("{origin}/api/on/{host}/workspaces"))
                        .send()
                        .unwrap()
                });
                wait_until(
                    &format!("{host}'s ssh to start"),
                    Duration::from_secs(10),
                    || stub.calls().len() > before,
                );
                let reserved = forwarded_port(&stub.calls()[before]);
                let foreign = TcpListener::bind((Ipv4Addr::LOCALHOST, reserved))
                    .expect("the reserved port is free until ssh binds it");
                let probes = serve_dashboard(foreign, Some(foreign_identity.clone()));

                let failed = error_code(request.join().unwrap(), 502, "unreachable_destination");
                assert_eq!(failed["host"], host, "{failed}");
                assert!(
                    failed["error"].as_str().unwrap().contains("exit 255"),
                    "{host}: ssh's own failure is reported: {failed}"
                );
                assert_eq!(
                    probes.load(Ordering::SeqCst),
                    0,
                    "{host}: nothing is probed through a port its ssh never bound"
                );
                assert_eq!(
                    stub.calls().len(),
                    before + 1,
                    "{host}: a failed probe spawns no remote dashboard"
                );
            }
        },
    );
}

#[test]
fn a_spawned_remote_is_operator_matched_and_stopped_on_shutdown() {
    isolated(
        "host_forward::a_spawned_remote_is_operator_matched_and_stopped_on_shutdown",
        || {
            let b = Fixture::host("hostb", "HB");
            let b_server = b.server(true);
            let a = Fixture::new();
            write_hosts(&a, &[("hostb", &b.machine_id(), "hostb-ssh", "HB")]);
            // Nothing answers the attach forward, so A spawns B's dashboard.
            let stub =
                StubSsh::new(json!({"hostb-ssh": {"attach": null, "spawn": port(&b_server)}}));
            let mut a_server = a.server_with_path(true, stub.bin());

            let state = json_ok(
                a_server
                    .request("GET", "/api/hosts/hostb/connection")
                    .timeout(SLOW_REQUEST)
                    .send()
                    .unwrap(),
            );
            assert_eq!(state["origin"], "spawned", "{state}");
            json_ok(a_server.get("/api/on/hostb/workspaces"));
            let calls = stub.calls();
            assert_eq!(calls.len(), 2, "attach probe, then spawn: {calls:?}");
            assert_eq!(
                calls[1]["command"],
                "orbit web serve --no-open --operator --port 7878"
            );
            let pids = stub.pids();
            assert!(pids.iter().any(|pid| alive(*pid)));

            assert!(a_server.terminate().success());
            for pid in pids {
                assert!(
                    !alive(pid),
                    "graceful shutdown stops the ssh child it started"
                );
            }
        },
    );
}

/// B's audit window walked through A: every page's ids and cursor match the
/// direct walk, so the forward keeps `x-audit-next-before`, and the walk
/// reaches each matching event once, newest first, with no cursor at the end.
#[test]
fn forwarded_audit_cursor_walks_the_remote_window_page_by_page() {
    isolated(
        "host_forward::forwarded_audit_cursor_walks_the_remote_window_page_by_page",
        || {
            let b = Fixture::host("hostb", "HB");
            let mut seeded = Vec::new();
            for index in 0..35 {
                let (id, tool, status) = if index < 12 {
                    (
                        format!("probe-{index}"),
                        "orbit.drain.probe",
                        AuditEventStatus::Success,
                    )
                } else if index < 15 {
                    (
                        format!("probe-fail-{index}"),
                        "orbit.drain.probe",
                        AuditEventStatus::Failure,
                    )
                } else {
                    (
                        format!("agent-{index}"),
                        "orbit.task.show",
                        AuditEventStatus::Success,
                    )
                };
                b.runtime
                    .record_audit_event(&audit_row(&id, Some(tool), status))
                    .unwrap();
                seeded.push((id, status));
            }
            let b_server = b.server(false);
            let a = Fixture::new();
            write_hosts(&a, &[("hostb", &b.machine_id(), "hostb-ssh", "HB")]);
            let stub =
                StubSsh::new(json!({"hostb-ssh": {"attach": port(&b_server), "spawn": null}}));
            let a_server = a.server_with_path(false, stub.bin());

            // The unfiltered SQL window, a Rust-side needle that pages through the
            // scan, and a SQL status filter whose three matches span two pages.
            type Keep = fn(&str, AuditEventStatus) -> bool;
            let cases: [(&str, usize, Keep); 3] = [
                ("", 8, |_, _| true),
                ("q=probe", 8, |id, _| id.starts_with("probe")),
                ("status=failure", 2, |_, status| {
                    status == AuditEventStatus::Failure
                }),
            ];
            for (query, limit, keep) in cases {
                let direct =
                    walk_audit_pages(|path| b_server.get(path), "/api/audit", query, limit);
                let forwarded = walk_audit_pages(
                    |path| a_server.get(path),
                    "/api/on/hostb/audit",
                    query,
                    limit,
                );
                assert_eq!(forwarded, direct, "{query}: forwarded pages match B direct");
                assert!(direct.len() > 1, "{query}: the walk followed a cursor");
                assert!(
                    forwarded.last().unwrap().1.is_none(),
                    "{query}: the window's last page carries no cursor"
                );
                let expected: Vec<String> = seeded
                    .iter()
                    .rev()
                    .filter(|(id, status)| keep(id, *status))
                    .map(|(id, _)| id.clone())
                    .collect();
                let reached: Vec<String> = forwarded.into_iter().flat_map(|(ids, _)| ids).collect();
                assert_eq!(reached, expected, "{query}: every match once, newest first");
            }
        },
    );
}

/// A scan that hits its cap on a page with no matches returns an empty page
/// that still carries a cursor. The forward must not drop it, or the rest of
/// the window stays unreachable from the host dashboard.
#[test]
fn forwarded_audit_cursor_survives_a_capped_short_page() {
    isolated(
        "host_forward::forwarded_audit_cursor_survives_a_capped_short_page",
        || {
            let b = Fixture::host("hostb", "HB");
            // More rows than one scan examines, none matching the needle.
            for index in 0..10_001 {
                b.runtime
                    .record_audit_event(&audit_row(
                        &format!("bulk-{index}"),
                        Some("orbit.task.show"),
                        AuditEventStatus::Success,
                    ))
                    .unwrap();
            }
            let b_server = b.server(false);
            let a = Fixture::new();
            write_hosts(&a, &[("hostb", &b.machine_id(), "hostb-ssh", "HB")]);
            let stub =
                StubSsh::new(json!({"hostb-ssh": {"attach": port(&b_server), "spawn": null}}));
            let a_server = a.server_with_path(false, stub.bin());

            let query = "q=no-such-needle";
            let direct = walk_audit_pages(|path| b_server.get(path), "/api/audit", query, 50);
            let forwarded =
                walk_audit_pages(|path| a_server.get(path), "/api/on/hostb/audit", query, 50);
            assert_eq!(forwarded, direct, "forwarded pages match B direct");
            let (first_ids, first_cursor) = &forwarded[0];
            assert!(
                first_ids.is_empty() && first_cursor.is_some(),
                "a capped short page forwards its cursor: {forwarded:?}"
            );
            assert!(forwarded.len() > 1, "the capped scan was resumed");
            assert!(
                forwarded.last().unwrap().1.is_none(),
                "the window's last page carries no cursor"
            );
        },
    );
}
