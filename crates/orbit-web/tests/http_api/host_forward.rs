//! `/api/on/:host/*rest` between two real dashboards. A serves; B is a second
//! fixture with its own root and machine identity; A's `ssh` is a stub with
//! OpenSSH's `-L` argv contract whose forward lands on B's real port.
//! Tunnel lifecycle (single flight, dead child, idle) is in the unit tests.

use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, TcpListener};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use orbit_common::test_fixtures::TEST_CODEX_MODEL;
use orbit_core::JobRunState;
use orbit_core::TaskStatus;
use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
use orbit_types::task::TaskArtifact;
use reqwest::blocking::Response;
use serde_json::{Value, json};

use super::support::{Fixture, Server, error_code, isolated, json_ok};

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
        let config = json!({"log": dir.path().join("calls.jsonl"), "hosts": hosts});
        fs::write(dir.path().join("ssh.json"), config.to_string()).unwrap();
        Self { dir }
    }

    fn bin(&self) -> &Path {
        self.dir.path()
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
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            let mut buf = [0u8; 2048];
            let n = stream.read(&mut buf).unwrap_or(0);
            let status = if buf[..n].starts_with(b"GET /healthz ") {
                "200 OK"
            } else {
                "404 Not Found"
            };
            let _ = write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
        }
    });
    port
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
                .timeout(Duration::from_secs(20))
                .send()
                .unwrap();
            assert_eq!(stream.status(), 200);
            assert_eq!(stream.headers()["content-type"], "text/event-stream");
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
            drop(agent);

            let operator = a.server_with_path(true, stub.bin());
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
                "dead-ssh": {"exit": 255, "delay": 1.5},
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
            let local = Instant::now();
            json_ok(a_server.get(&format!("/api/tasks{WS}")));
            assert!(
                local.elapsed() < Duration::from_secs(1),
                "a slow host does not stall local requests"
            );
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
