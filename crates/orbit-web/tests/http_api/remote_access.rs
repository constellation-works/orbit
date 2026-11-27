use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, Shutdown, TcpListener, TcpStream};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use orbit_common::process::ancestry::process_start_key;
use reqwest::blocking::Response;
use serde_json::{Value, json};

use super::support::{Fixture, Server, isolated};

struct SshStubCleanup {
    log: std::path::PathBuf,
}

impl Drop for SshStubCleanup {
    fn drop(&mut self) {
        let Ok(text) = fs::read_to_string(&self.log) else {
            return;
        };
        for line in text.lines() {
            let Ok(call) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let (Some(pid), Some(starttime)) = (call["pid"].as_u64(), call["starttime"].as_u64())
            else {
                continue;
            };
            let Ok(pid) = u32::try_from(pid) else {
                continue;
            };
            if process_start_key(pid).is_some_and(|key| key.starttime == starttime) {
                // SAFETY: `process_start_key` confirmed the PID still belongs to the stub.
                unsafe {
                    libc::kill(pid as libc::pid_t, libc::SIGKILL);
                }
            }
        }
    }
}

struct PendingUpstream {
    release: mpsc::Sender<()>,
    stream_rx: mpsc::Receiver<TcpStream>,
}

/// Releases the `serve status` probes a [`gated_tailscale_stub`] holds. Dropping
/// it releases any probe a failed test left blocked, so no stub outlives it.
struct Gate(std::path::PathBuf);

impl Gate {
    /// Lets every probe waiting at the gate finish. Fails when none is waiting,
    /// since a non-blocking open for writing needs a reader.
    fn release(&self) -> std::io::Result<()> {
        let mut gate = fs::OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&self.0)?;
        gate.write_all(&[b'\n'; 16])
    }
}

impl Drop for Gate {
    fn drop(&mut self) {
        let _ = self.release();
    }
}

struct FakeRemote {
    port: u16,
    pending_rx: mpsc::Receiver<PendingUpstream>,
    immediate_rx: mpsc::Receiver<TcpStream>,
    hold_releases: Arc<Mutex<Vec<mpsc::Sender<()>>>>,
}

impl FakeRemote {
    fn start(machine_id: &str) -> Self {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let (pending_tx, pending_rx) = mpsc::channel();
        let (immediate_tx, immediate_rx) = mpsc::channel();
        let hold_releases = Arc::new(Mutex::new(Vec::new()));
        let releases = Arc::clone(&hold_releases);
        let identity = json!({"hosts": [{
            "local": true,
            "machine_id": machine_id,
            "binary_version": "9.9.9",
            "protocol_fingerprint": "fixture",
        }]})
        .to_string();
        thread::spawn(move || {
            for incoming in listener.incoming() {
                let Ok(stream) = incoming else { break };
                let pending_tx = pending_tx.clone();
                let immediate_tx = immediate_tx.clone();
                let releases = Arc::clone(&releases);
                let identity = identity.clone();
                thread::spawn(move || {
                    handle_remote_connection(stream, &identity, pending_tx, immediate_tx, releases);
                });
            }
        });
        Self {
            port,
            pending_rx,
            immediate_rx,
            hold_releases,
        }
    }
}

impl Drop for FakeRemote {
    fn drop(&mut self) {
        for tx in self.hold_releases.lock().unwrap().drain(..) {
            let _ = tx.send(());
        }
    }
}

/// A `tailscale` stub: `serve status --json` prints `serve.json` beside it,
/// `whois --json <ip>` names the device `phone`.
fn tailscale_stub(fixture: &Fixture) -> std::path::PathBuf {
    let bin = fixture.path("bin");
    fs::create_dir_all(&bin).unwrap();
    let script = bin.join("tailscale");
    fs::write(
        &script,
        "#!/bin/sh\ncase \"$1\" in\n\
         serve) cat \"$(dirname \"$0\")/serve.json\" ;;\n\
         whois) echo '{\"Node\":{\"ComputedName\":\"phone\",\"Name\":\"phone.tailnet.ts.net.\"},\"UserProfile\":{\"LoginName\":\"alice@example.com\"}}' ;;\n\
         *) exit 2 ;;\nesac\n",
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

/// Point Serve's `<name>` handler at `target_host` on the server's own port.
fn serve_json(bin: &std::path::Path, name: &str, origin: &str, target_host: &str) {
    let port = origin.rsplit(':').next().unwrap();
    fs::write(
        bin.join("serve.json"),
        json!({
            "TCP": {"443": {"HTTPS": true}},
            "Web": {format!("{name}:443"): {"Handlers": {"/": {"Proxy": format!("http://{target_host}:{port}")}}}},
        })
        .to_string(),
    )
    .unwrap();
}

#[test]
fn remote_access_reports_policy_serve_check_and_only_trusted_host_traffic() {
    isolated(
        "remote_access::remote_access_reports_policy_serve_check_and_only_trusted_host_traffic",
        || {
            // Off: no trusted hosts; Serve proxying here is flagged.
            let fixture = Fixture::new();
            let bin = tailscale_stub(&fixture);
            let off = fixture.server_with_path(false, &bin);
            serve_json(&bin, "dash.tailnet.ts.net", &off.origin, "127.0.0.1");
            let body = super::support::json_ok(off.get("/api/remote-access"));
            assert_eq!(body["enabled"], false, "{body}");
            assert_eq!(body["trusted_hosts"], json!([]));
            assert_eq!(body["restart_required"], false);
            assert_eq!(body["configured_policy_error"], Value::Null);
            assert_eq!(body["tailscale"]["state"], "ok", "{body}");
            assert_eq!(
                body["tailscale"]["handlers"][0]["to_dashboard"], true,
                "{body}"
            );
            assert_eq!(
                body["warnings"],
                json!([{"code": "serve_host_not_trusted", "host": "dash.tailnet.ts.net"}])
            );
            assert_eq!(body["connections"]["devices"], json!([]));
            assert_eq!(body["refusals"]["total"], 0);
            drop(off);

            // On: one trusted host and login.
            let config = fixture.global.join("config.toml");
            let base = fs::read_to_string(&config).unwrap();
            fs::write(
                &config,
                format!(
                    "{base}\n[machine.web]\ntrusted_hosts = [\"dash.tailnet.ts.net\"]\n\
                     trusted_logins = [\"alice@example.com\"]\n"
                ),
            )
            .unwrap();
            fs::remove_file(bin.join("serve.json")).unwrap();
            let on = fixture.server_with_path(false, &bin);
            serve_json(&bin, "dash.tailnet.ts.net", &on.origin, "127.0.0.1");
            let api = "/api/tasks?workspace=ws_http_fixture";
            let trusted = |login: &str| {
                on.request("GET", api)
                    .header("host", "dash.tailnet.ts.net")
                    .header("tailscale-user-login", login)
                    .header("x-forwarded-for", "100.64.0.7")
                    .send()
                    .unwrap()
            };
            // Loopback traffic is never counted.
            assert!(on.get(api).status().is_success());
            assert!(trusted("alice@example.com").status().is_success());
            assert_eq!(trusted("mallory@example.com").status().as_u16(), 403);

            let body = super::support::json_ok(on.get("/api/remote-access"));
            assert_eq!(body["enabled"], true, "{body}");
            assert_eq!(
                body["trusted_hosts"],
                json!([{"host": "dash.tailnet.ts.net", "url": "https://dash.tailnet.ts.net"}])
            );
            assert_eq!(body["trusted_logins"], json!(["alice@example.com"]));
            assert_eq!(body["restart_required"], false);
            assert_eq!(body["configured_policy_error"], Value::Null);
            assert_eq!(body["warnings"], json!([]), "{body}");
            let connections = &body["connections"];
            assert!(connections["tracking_since"].is_string(), "{body}");
            assert_eq!(connections["active_devices"], 1);
            assert_eq!(connections["open_streams"], 0);
            let devices = connections["devices"].as_array().unwrap();
            assert_eq!(devices.len(), 1, "{body}");
            assert_eq!(devices[0]["ip"], "100.64.0.7");
            assert_eq!(devices[0]["device"], "phone");
            assert_eq!(devices[0]["login"], "alice@example.com");
            assert_eq!(devices[0]["requests_15m"], 1);
            let refusals = &body["refusals"];
            assert_eq!(refusals["total"], 1);
            let items: Vec<Value> = refusals["items"].as_array().unwrap().clone();
            assert_eq!(items.len(), 1);
            assert_eq!(items[0]["reason"], "unlisted_login");
            assert_eq!(items[0]["login"], "mallory@example.com");
            assert_eq!(items[0]["count_24h"], 1);

            // An incomplete saved pair cannot start the dashboard. Keep
            // showing the running pair and direct the operator to add a login.
            fs::write(
                &config,
                format!("{base}\n[machine.web]\ntrusted_hosts = [\"new.tailnet.ts.net\"]\n"),
            )
            .unwrap();
            let body = super::support::json_ok(on.get("/api/remote-access"));
            assert_eq!(
                body["trusted_hosts"],
                json!([{"host": "dash.tailnet.ts.net", "url": "https://dash.tailnet.ts.net"}]),
                "{body}"
            );
            assert_eq!(body["trusted_logins"], json!(["alice@example.com"]));
            assert_eq!(
                body["configured"]["trusted_hosts"],
                json!(["new.tailnet.ts.net"])
            );
            assert_eq!(body["configured"]["trusted_logins"], json!([]));
            assert_eq!(body["restart_required"], false, "{body}");
            assert!(
                body["configured_policy_error"]
                    .as_str()
                    .is_some_and(|error| error.contains("trusted_logins is empty")),
                "{body}"
            );

            // Completing the pair makes the save restart-ready. The running
            // policy remains visible until the process restarts.
            fs::write(
                &config,
                format!(
                    "{base}\n[machine.web]\ntrusted_hosts = [\"new.tailnet.ts.net\"]\n\
                     trusted_logins = [\"bob@example.com\"]\n"
                ),
            )
            .unwrap();
            let body = super::support::json_ok(on.get("/api/remote-access"));
            assert_eq!(
                body["trusted_hosts"],
                json!([{"host": "dash.tailnet.ts.net", "url": "https://dash.tailnet.ts.net"}]),
                "{body}"
            );
            assert_eq!(body["trusted_logins"], json!(["alice@example.com"]));
            assert_eq!(
                body["configured"]["trusted_hosts"],
                json!(["new.tailnet.ts.net"])
            );
            assert_eq!(
                body["configured"]["trusted_logins"],
                json!(["bob@example.com"])
            );
            assert_eq!(body["restart_required"], true, "{body}");
            assert_eq!(body["configured_policy_error"], Value::Null);
        },
    );
}

/// ORB-28659: a Serve target counts as this dashboard only on the listener's
/// own loopback family. An explicit `127.0.0.1` or `[::1]` on the other family
/// is a different socket even on the same port, so it must not be reported as
/// the dashboard, and the trusted host must read as not served.
#[test]
fn remote_access_serve_target_must_match_the_listener_family() {
    isolated(
        "remote_access::remote_access_serve_target_must_match_the_listener_family",
        || {
            // (listener host, Serve target host, Serve reaches this dashboard)
            let cases = [
                ("127.0.0.1", "127.0.0.1", true),
                ("127.0.0.1", "[::1]", false),
                ("::1", "[::1]", true),
                ("::1", "127.0.0.1", false),
            ];
            for (listener, target, reaches) in cases {
                let fixture = Fixture::new();
                let bin = tailscale_stub(&fixture);
                let config = fixture.global.join("config.toml");
                let base = fs::read_to_string(&config).unwrap();
                fs::write(
                    &config,
                    format!(
                        "{base}\n[machine.web]\ntrusted_hosts = [\"dash.tailnet.ts.net\"]\n\
                         trusted_logins = [\"alice@example.com\"]\n"
                    ),
                )
                .unwrap();
                let server = fixture.server_with_path_envs(
                    false,
                    &bin,
                    &[("ORBIT_HTTP_HOST", std::ffi::OsStr::new(listener))],
                );
                serve_json(&bin, "dash.tailnet.ts.net", &server.origin, target);

                let body = super::support::json_ok(server.get("/api/remote-access"));
                let case = format!("listener {listener}, Serve target {target}");
                assert_eq!(body["tailscale"]["state"], "ok", "{case}: {body}");
                assert_eq!(
                    body["tailscale"]["handlers"][0]["to_dashboard"], reaches,
                    "{case}: {body}"
                );
                let warnings = if reaches {
                    json!([])
                } else {
                    json!([{"code": "trusted_host_not_served", "host": "dash.tailnet.ts.net"}])
                };
                assert_eq!(body["warnings"], warnings, "{case}: {body}");
            }
        },
    );
}

fn install_ssh_stub(bin: &Path, remote_port: u16) -> SshStubCleanup {
    let script = bin.join("ssh");
    fs::write(&script, include_str!("stub_ssh.py")).unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let log = bin.join("calls.jsonl");
    let config = json!({
        "log": log,
        "release": bin.join("release"),
        "hosts": {"hostb-ssh": {"attach": remote_port, "spawn": null}},
    });
    fs::write(bin.join("ssh.json"), config.to_string()).unwrap();
    SshStubCleanup { log }
}

fn handle_remote_connection(
    mut stream: TcpStream,
    identity: &str,
    pending_tx: mpsc::Sender<PendingUpstream>,
    immediate_tx: mpsc::Sender<TcpStream>,
    hold_releases: Arc<Mutex<Vec<mpsc::Sender<()>>>>,
) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        let Ok(read) = stream.read(&mut chunk) else {
            return;
        };
        if read == 0 {
            return;
        }
        buf.extend_from_slice(&chunk[..read]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.len() > 16 * 1024 {
            break;
        }
    }
    let _ = stream.set_read_timeout(None);
    let head = String::from_utf8_lossy(&buf);
    let path = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or_default();
    if path == "/healthz" {
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
    } else if path == "/api/hosts?probe=false" {
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{identity}",
            identity.len()
        );
    } else if path.starts_with("/api/events/stream?mode=pending") {
        let (release_tx, release_rx) = mpsc::channel();
        hold_releases.lock().unwrap().push(release_tx.clone());
        let (stream_tx, stream_rx) = mpsc::channel();
        if pending_tx
            .send(PendingUpstream {
                release: release_tx,
                stream_rx,
            })
            .is_err()
        {
            return;
        }
        if release_rx.recv().is_err() {
            return;
        }
        if write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\n\r\ndata: {{\"kind\":\"ready\"}}\n\n"
        )
        .is_err()
            || stream.flush().is_err()
        {
            return;
        }
        let _ = stream_tx.send(stream);
    } else if path.starts_with("/api/events/stream") {
        if write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\n\r\ndata: {{\"kind\":\"ready\"}}\n\n"
        )
        .is_err()
            || stream.flush().is_err()
        {
            return;
        }
        let _ = immediate_tx.send(stream);
    } else {
        let _ = write!(
            stream,
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
    }
}

fn configure_trusted_remote_host(fixture: &Fixture, bin: &Path) {
    let config = fixture.global.join("config.toml");
    let base = fs::read_to_string(&config).unwrap();
    fs::write(
        &config,
        format!(
            "{base}\n[machine.web]\ntrusted_hosts = [\"dash.tailnet.ts.net\"]\n\
             trusted_logins = [\"alice@example.com\"]\n"
        ),
    )
    .unwrap();
    fs::write(
        fixture.global.join("hosts.toml"),
        "schema_version = 1\n\n[[hosts]]\nname = \"hostb\"\nmachine_id = \"hm_hostb_remote\"\nssh = \"hostb-ssh\"\ntask_prefix = \"HB\"\n",
    )
    .unwrap();
    serve_json(
        bin,
        "dash.tailnet.ts.net",
        "http://127.0.0.1:0",
        "127.0.0.1",
    );
}

fn send_trusted(server: &Server, path: &str, login: &str, ip: &str) -> Response {
    server
        .request("GET", path)
        .header("host", "dash.tailnet.ts.net")
        .header("tailscale-user-login", login)
        .header("x-forwarded-for", ip)
        .timeout(Duration::from_secs(20))
        .send()
        .unwrap()
}

fn read_sse_ready(response: Response) -> BufReader<Response> {
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    let mut reader = BufReader::new(response);
    let mut line = String::new();
    loop {
        line.clear();
        assert!(
            reader.read_line(&mut line).unwrap() > 0,
            "SSE stream ended before initial event"
        );
        if line.starts_with("data: ") {
            return reader;
        }
    }
}

fn close_and_drain(mut reader: BufReader<Response>, upstream: TcpStream) {
    let _ = upstream.shutdown(Shutdown::Both);
    drop(upstream);
    let mut rest = String::new();
    while reader.read_line(&mut rest).is_ok_and(|n| n > 0) {}
}

#[test]
fn remote_access_caps_devices_and_refusal_keys_at_capacity() {
    isolated(
        "remote_access::remote_access_caps_devices_and_refusal_keys_at_capacity",
        || {
            let fixture = Fixture::new();
            let bin = tailscale_stub(&fixture);
            let remote = FakeRemote::start("hm_hostb_remote");
            let _ssh = install_ssh_stub(&bin, remote.port);
            configure_trusted_remote_host(&fixture, &bin);
            let server = fixture.server_with_path(false, &bin);
            serve_json(&bin, "dash.tailnet.ts.net", &server.origin, "127.0.0.1");

            // Open 64 live forwarded SSE streams from 64 distinct devices.
            let mut open_streams = Vec::new();
            for i in 1..=64 {
                let ip = format!("100.64.2.{i}");
                let response = send_trusted(
                    &server,
                    "/api/on/hostb/events/stream?mode=immediate",
                    "alice@example.com",
                    &ip,
                );
                let reader = read_sse_ready(response);
                let upstream = remote
                    .immediate_rx
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap();
                open_streams.push((ip, reader, upstream));
            }

            let body = super::support::json_ok(server.get("/api/remote-access"));
            assert_eq!(body["connections"]["devices"].as_array().unwrap().len(), 64);
            assert_eq!(body["connections"]["open_streams"], 64);

            // A 65th ordinary request and a 65th SSE stream cannot grow the table beyond 64.
            assert!(
                send_trusted(
                    &server,
                    "/api/tasks?workspace=ws_http_fixture",
                    "alice@example.com",
                    "100.64.2.65",
                )
                .status()
                .is_success()
            );
            let overflow_sse = read_sse_ready(send_trusted(
                &server,
                "/api/on/hostb/events/stream?mode=immediate",
                "alice@example.com",
                "100.64.2.65",
            ));
            let overflow_upstream = remote
                .immediate_rx
                .recv_timeout(Duration::from_secs(5))
                .unwrap();

            let body = super::support::json_ok(server.get("/api/remote-access"));
            let devices = body["connections"]["devices"].as_array().unwrap();
            assert_eq!(devices.len(), 64, "device table must remain capped at 64");
            assert_eq!(body["connections"]["open_streams"], 64);
            assert!(
                !devices.iter().any(|d| d["ip"] == "100.64.2.65"),
                "unretained 65th device must not be inserted while all 64 slots hold streams"
            );

            // Close one pinned device's stream, then let 100.64.2.65 open a newly accounted stream.
            let (_, first_reader, first_upstream) = open_streams.remove(0);
            close_and_drain(first_reader, first_upstream);
            let body = super::support::json_ok(server.get("/api/remote-access"));
            assert_eq!(body["connections"]["open_streams"], 63);

            let accounted_65_reader = read_sse_ready(send_trusted(
                &server,
                "/api/on/hostb/events/stream?mode=immediate",
                "alice@example.com",
                "100.64.2.65",
            ));
            let accounted_65_upstream = remote
                .immediate_rx
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
            open_streams.push((
                "100.64.2.65".to_string(),
                accounted_65_reader,
                accounted_65_upstream,
            ));

            // Closing the earlier unaccounted 65th stream must not decrement the new accounted one.
            close_and_drain(overflow_sse, overflow_upstream);
            let body = super::support::json_ok(server.get("/api/remote-access"));
            assert_eq!(body["connections"]["open_streams"], 64);
            let dev_65 = body["connections"]["devices"]
                .as_array()
                .unwrap()
                .iter()
                .find(|d| d["ip"] == "100.64.2.65")
                .expect("100.64.2.65 should now be retained");
            assert_eq!(dev_65["open_streams"], 1);

            // Refusal key capacity: 64 unlisted logins fill the table, then overflow and other reasons
            // must not grow the refusal items list beyond 64.
            for i in 0..64 {
                let login = format!("mallory-{i:02}@example.com");
                assert_eq!(
                    send_trusted(
                        &server,
                        "/api/tasks?workspace=ws_http_fixture",
                        &login,
                        "100.64.3.1",
                    )
                    .status()
                    .as_u16(),
                    403
                );
            }
            let body = super::support::json_ok(server.get("/api/remote-access"));
            assert_eq!(body["refusals"]["items"].as_array().unwrap().len(), 64);
            assert_eq!(body["refusals"]["total"], 64);

            // A new reason arrives before an unlisted-login overflow bucket exists.
            assert_eq!(
                server
                    .request("GET", "/api/tasks?workspace=ws_http_fixture")
                    .header("host", "dash.tailnet.ts.net")
                    .send()
                    .unwrap()
                    .status()
                    .as_u16(),
                403
            );
            let body = super::support::json_ok(server.get("/api/remote-access"));
            let items = body["refusals"]["items"].as_array().unwrap();
            assert!(items.len() <= 64);
            assert_eq!(body["refusals"]["total"], 65);
            assert_eq!(
                items
                    .iter()
                    .filter(|item| item["reason"] == "unlisted_login")
                    .map(|item| item["count_24h"].as_u64().unwrap())
                    .sum::<u64>(),
                64
            );

            for i in 64..66 {
                let login = format!("mallory-{i:02}@example.com");
                assert_eq!(
                    send_trusted(
                        &server,
                        "/api/tasks?workspace=ws_http_fixture",
                        &login,
                        "100.64.3.1",
                    )
                    .status()
                    .as_u16(),
                    403
                );
            }
            assert_eq!(
                server
                    .request("GET", "/api/tasks?workspace=ws_http_fixture")
                    .header("host", "dash.tailnet.ts.net")
                    .header("tailscale-user-login", "alice@example.com")
                    .header("origin", "https://evil.example")
                    .send()
                    .unwrap()
                    .status()
                    .as_u16(),
                403
            );
            assert_eq!(
                server
                    .request("GET", "/api/tasks?workspace=ws_http_fixture")
                    .header("host", "dash.tailnet.ts.net")
                    .header("tailscale-user-login", "alice@example.com")
                    .header("sec-fetch-site", "cross-site")
                    .send()
                    .unwrap()
                    .status()
                    .as_u16(),
                403
            );

            let body = super::support::json_ok(server.get("/api/remote-access"));
            let items = body["refusals"]["items"].as_array().unwrap();
            assert!(
                items.len() <= 64,
                "refusal keys must remain capped at 64, got {}: {items:?}",
                items.len()
            );
            assert_eq!(body["refusals"]["total"], 69);
            assert!(
                items
                    .iter()
                    .any(|item| item["reason"] == "unlisted_login" && item["login"].is_null()),
                "overflow unlisted_login bucket should be present: {items:?}"
            );
            assert!(
                items.iter().any(|item| item["reason"] == "origin_mismatch"),
                "origin_mismatch bucket should be present: {items:?}"
            );
            assert!(
                items.iter().any(|item| item["reason"] == "sec_fetch_site"),
                "sec_fetch_site bucket should be present: {items:?}"
            );

            for (_, reader, upstream) in open_streams {
                close_and_drain(reader, upstream);
            }
            let body = super::support::json_ok(server.get("/api/remote-access"));
            assert_eq!(body["connections"]["open_streams"], 0);
        },
    );
}

/// A `tailscale` stub that logs each call to `calls.log`. Its `serve status`
/// blocks reading the `gate` FIFO, so the test decides when that probe ends.
fn gated_tailscale_stub(fixture: &Fixture) -> (std::path::PathBuf, Gate) {
    let bin = fixture.path("bin");
    fs::create_dir_all(&bin).unwrap();
    let script = bin.join("tailscale");
    fs::write(
        &script,
        r#"#!/bin/sh
dir="$(dirname "$0")"
case "$1" in
serve)
    echo serve >> "$dir/calls.log"
    read -r _ < "$dir/gate"
    cat "$dir/serve.json" ;;
whois)
    echo "whois $3" >> "$dir/calls.log"
    echo '{"Node":{"ComputedName":"phone","Name":"phone.tailnet.ts.net."},"UserProfile":{"LoginName":"alice@example.com"}}' ;;
*) exit 2 ;;
esac
"#,
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let gate = bin.join("gate");
    let c_gate = std::ffi::CString::new(gate.as_os_str().as_bytes()).unwrap();
    // SAFETY: `c_gate` is a NUL-terminated path that outlives the call.
    let made = unsafe { libc::mkfifo(c_gate.as_ptr(), 0o600) };
    assert_eq!(made, 0, "mkfifo: {}", std::io::Error::last_os_error());
    (bin, Gate(gate))
}

fn tailscale_calls(bin: &Path) -> Vec<String> {
    fs::read_to_string(bin.join("calls.log"))
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

fn spawn_remote_access_read(origin: &str) -> thread::JoinHandle<Response> {
    let url = format!("{origin}/api/remote-access");
    thread::spawn(move || {
        reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()
            .unwrap()
            .get(url)
            .send()
            .unwrap()
    })
}

/// ORB-28660: overlapping `GET /api/remote-access` reads share one in-flight
/// Serve probe and one whois probe per device. A read that misses the cache
/// while a probe is running waits for that probe instead of starting another
/// `tailscale` process.
#[test]
fn remote_access_overlapping_reads_share_one_tailscale_probe() {
    isolated(
        "remote_access::remote_access_overlapping_reads_share_one_tailscale_probe",
        || {
            let fixture = Fixture::new();
            let (bin, gate) = gated_tailscale_stub(&fixture);
            let config = fixture.global.join("config.toml");
            let base = fs::read_to_string(&config).unwrap();
            fs::write(
                &config,
                format!(
                    "{base}\n[machine.web]\ntrusted_hosts = [\"dash.tailnet.ts.net\"]\n\
                     trusted_logins = [\"alice@example.com\"]\n"
                ),
            )
            .unwrap();
            let server = fixture.server_with_path(false, &bin);
            serve_json(&bin, "dash.tailnet.ts.net", &server.origin, "127.0.0.1");
            // One trusted request records the device, so each read names it through whois.
            assert!(
                send_trusted(
                    &server,
                    "/api/tasks?workspace=ws_http_fixture",
                    "alice@example.com",
                    "100.64.0.7",
                )
                .status()
                .is_success()
            );

            // The first read starts the serve probe, which the stub holds at the gate.
            let first = spawn_remote_access_read(&server.origin);
            let waited_since = Instant::now();
            while !tailscale_calls(&bin)
                .iter()
                .any(|call| call.as_str() == "serve")
            {
                assert!(
                    waited_since.elapsed() < Duration::from_secs(10),
                    "the first read never started a serve probe"
                );
                thread::sleep(Duration::from_millis(10));
            }

            // Overlapping reads arrive while that probe is still held.
            let overlapping: Vec<_> = (0..2)
                .map(|_| spawn_remote_access_read(&server.origin))
                .collect();
            thread::sleep(Duration::from_millis(300));
            let calls = tailscale_calls(&bin);
            assert_eq!(
                calls.iter().filter(|call| call.as_str() == "serve").count(),
                1,
                "overlapping reads must wait on the probe in flight: {calls:?}"
            );

            gate.release().unwrap();
            for response in std::iter::once(first).chain(overlapping) {
                let body = super::support::json_ok(response.join().unwrap());
                assert_eq!(
                    body["connections"]["devices"][0]["device"], "phone",
                    "{body}"
                );
            }
            let calls = tailscale_calls(&bin);
            assert_eq!(
                calls.iter().filter(|call| call.as_str() == "serve").count(),
                1,
                "{calls:?}"
            );
            assert_eq!(
                calls
                    .iter()
                    .filter(|call| call.as_str() == "whois 100.64.0.7")
                    .count(),
                1,
                "{calls:?}"
            );
        },
    );
}

#[test]
fn remote_access_overlapping_reads_bound_whois_waits() {
    isolated(
        "remote_access::remote_access_overlapping_reads_bound_whois_waits",
        || {
            let fixture = Fixture::new();
            let (bin, gate) = gated_tailscale_stub(&fixture);
            let script = bin.join("tailscale");
            let source = fs::read_to_string(&script).unwrap().replace(
            "echo \"whois $3\" >> \"$dir/calls.log\"",
            "echo \"whois $3\" >> \"$dir/calls.log\"\n    if [ \"$3\" = 100.64.0.9 ]; then read -r _ < \"$dir/gate\"; fi",
        );
            fs::write(&script, source).unwrap();
            let config = fixture.global.join("config.toml");
            let base = fs::read_to_string(&config).unwrap();
            fs::write(&config, format!("{base}\n[machine.web]\ntrusted_hosts = [\"dash.tailnet.ts.net\"]\ntrusted_logins = [\"alice@example.com\"]\n")).unwrap();
            let server = fixture.server_with_path(false, &bin);
            serve_json(&bin, "dash.tailnet.ts.net", &server.origin, "127.0.0.1");
            let record = |ip: &str| {
                assert!(
                    send_trusted(
                        &server,
                        "/api/tasks?workspace=ws_http_fixture",
                        "alice@example.com",
                        ip
                    )
                    .status()
                    .is_success()
                );
            };
            record("100.64.0.9");
            let first = spawn_remote_access_read(&server.origin);
            let wait_for = |expected: &str| {
                let deadline = Instant::now() + Duration::from_secs(10);
                while !tailscale_calls(&bin).iter().any(|call| call == expected) {
                    assert!(Instant::now() < deadline, "missing call {expected}");
                    thread::sleep(Duration::from_millis(10));
                }
            };
            wait_for("serve");
            gate.release().unwrap();
            wait_for("whois 100.64.0.9");

            // The first snapshot contains only .9, whose probe remains held. The
            // second snapshot sees eight newer devices before the in-flight .9.
            for i in 1..=8 {
                record(&format!("100.64.0.{i}"));
            }
            let origin = server.origin.clone();
            let (tx, rx) = mpsc::channel();
            let second = thread::spawn(move || {
                tx.send(spawn_remote_access_read(&origin).join().unwrap())
                    .unwrap();
            });
            let response = rx
                .recv_timeout(Duration::from_secs(2))
                .expect("request must not wait on a ninth uncached probe");
            second.join().unwrap();
            let body = super::support::json_ok(response);
            let devices = body["connections"]["devices"].as_array().unwrap();
            assert_eq!(
                devices
                    .iter()
                    .filter(|device| !device["device"].is_null())
                    .count(),
                8,
                "{body}"
            );
            assert!(
                devices
                    .iter()
                    .any(|device| device["ip"] == "100.64.0.9" && device["device"].is_null()),
                "{body}"
            );
            let calls = tailscale_calls(&bin);
            let whois: Vec<_> = calls
                .iter()
                .filter(|call| call.starts_with("whois "))
                .collect();
            assert_eq!(whois.len(), 9, "{calls:?}");
            assert_eq!(
                whois.iter().collect::<std::collections::HashSet<_>>().len(),
                9,
                "one subprocess per IP: {calls:?}"
            );
            gate.release().unwrap();
            assert_eq!(
                super::support::json_ok(first.join().unwrap())["connections"]["devices"][0]["device"],
                "phone"
            );

            // All nine cached names fit even though the request budget is eight.
            let body = super::support::json_ok(server.get("/api/remote-access"));
            assert_eq!(
                body["connections"]["devices"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|device| !device["device"].is_null())
                    .count(),
                9
            );
            assert_eq!(
                tailscale_calls(&bin)
                    .iter()
                    .filter(|call| call.starts_with("whois "))
                    .count(),
                9
            );
        },
    );
}

#[test]
fn remote_access_evicted_pending_stream_cannot_decrement_later_stream() {
    isolated(
        "remote_access::remote_access_evicted_pending_stream_cannot_decrement_later_stream",
        || {
            let fixture = Fixture::new();
            let bin = tailscale_stub(&fixture);
            let remote = FakeRemote::start("hm_hostb_remote");
            let _ssh = install_ssh_stub(&bin, remote.port);
            configure_trusted_remote_host(&fixture, &bin);
            let server = fixture.server_with_path(false, &bin);
            serve_json(&bin, "dash.tailnet.ts.net", &server.origin, "127.0.0.1");

            // Start a forwarded SSE request that pauses before returning response headers.
            let origin = server.origin.clone();
            let pending_handle = thread::spawn(move || {
                let client = reqwest::blocking::Client::builder()
                    .timeout(Duration::from_secs(20))
                    .build()
                    .unwrap();
                client
                    .get(format!("{origin}/api/on/hostb/events/stream?mode=pending"))
                    .header("host", "dash.tailnet.ts.net")
                    .header("tailscale-user-login", "alice@example.com")
                    .header("x-forwarded-for", "100.64.0.99")
                    .send()
                    .unwrap()
            });

            // Once the fake remote receives the request, Gate::admit has recorded 100.64.0.99
            // and is suspended awaiting the handler response.
            let pending = remote
                .pending_rx
                .recv_timeout(Duration::from_secs(10))
                .expect("pending stream reached remote");

            // Fill all 64 device slots with ordinary requests, evicting 100.64.0.99 while pending.
            for i in 1..=64 {
                let ip = format!("100.64.1.{i}");
                assert!(
                    send_trusted(
                        &server,
                        "/api/tasks?workspace=ws_http_fixture",
                        "alice@example.com",
                        &ip,
                    )
                    .status()
                    .is_success()
                );
            }
            let body = super::support::json_ok(server.get("/api/remote-access"));
            let devices = body["connections"]["devices"].as_array().unwrap();
            assert_eq!(devices.len(), 64);
            assert!(
                !devices.iter().any(|d| d["ip"] == "100.64.0.99"),
                "pending device should have been evicted by 64 ordinary requests"
            );

            // Release the pending request so it returns text/event-stream and stays open.
            pending.release.send(()).unwrap();
            let pending_reader = read_sse_ready(pending_handle.join().unwrap());
            let pending_upstream = pending
                .stream_rx
                .recv_timeout(Duration::from_secs(5))
                .unwrap();

            // Open a second SSE stream from the same IP while the first is still open.
            let second_reader = read_sse_ready(send_trusted(
                &server,
                "/api/on/hostb/events/stream?mode=immediate",
                "alice@example.com",
                "100.64.0.99",
            ));
            let second_upstream = remote
                .immediate_rx
                .recv_timeout(Duration::from_secs(5))
                .unwrap();

            // Close the first stream; the second stream must remain counted as 1.
            close_and_drain(pending_reader, pending_upstream);
            let body = super::support::json_ok(server.get("/api/remote-access"));
            assert_eq!(body["connections"]["open_streams"], 1);
            let dev = body["connections"]["devices"]
                .as_array()
                .unwrap()
                .iter()
                .find(|d| d["ip"] == "100.64.0.99")
                .expect("100.64.0.99 should remain present for its second stream");
            assert_eq!(dev["open_streams"], 1);

            // Closing the second stream releases its own count cleanly to 0.
            close_and_drain(second_reader, second_upstream);
            let body = super::support::json_ok(server.get("/api/remote-access"));
            assert_eq!(body["connections"]["open_streams"], 0);
        },
    );
}
