//! Host-forward tunnel lifecycle [ORB-14679]: single-flight establish,
//! re-establish after a dead child, idle teardown, and shutdown. Identity,
//! authority, the spawned remote's operator argv, and HTTP failures are
//! covered through the router in `tests/http_api/host_forward.rs`.
//!
//! Admitted as unit tests (deterministic interleaving and child-process
//! fault injection): the HTTP surface cannot shorten the idle timer, count
//! concurrent establishes, or kill one tunnel's child. The two-dashboard
//! HTTP contract lives in `tests/http_api/host_forward.rs`; both use the same
//! stub, a real executable with ssh's `-L` argv contract.

use std::fs;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpListener};
use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Barrier, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::host_tunnels::{HostTarget, HostTunnels, TunnelConfig};

const STUB_SSH: &str = include_str!("../../tests/http_api/stub_ssh.py");
const MACHINE_ID: &str = "hm_unit_remote";

/// The stub's directory and log. Dropping it kills every stub it logged, so a
/// failed assertion cannot leak one.
struct Stub {
    dir: tempfile::TempDir,
}

impl Stub {
    fn new(hosts: Value) -> Self {
        let dir = tempfile::tempdir().expect("stub dir");
        let script = dir.path().join("ssh");
        fs::write(&script, STUB_SSH).expect("write ssh stub");
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("chmod ssh stub");
        let config = json!({"log": dir.path().join("calls.jsonl"), "hosts": hosts});
        fs::write(dir.path().join("ssh.json"), config.to_string()).expect("write stub config");
        Self { dir }
    }

    fn program(&self) -> String {
        self.dir.path().join("ssh").display().to_string()
    }

    fn calls(&self) -> Vec<Value> {
        fs::read_to_string(self.dir.path().join("calls.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).expect("stub call record"))
            .collect()
    }
}

impl Drop for Stub {
    fn drop(&mut self) {
        if thread::panicking() {
            for name in ["calls.jsonl", "bind-events.jsonl", "phases.jsonl"] {
                // Diagnostic I/O must not interrupt child cleanup during unwind.
                let _ = writeln!(
                    std::io::stderr(),
                    "SSH fixture {name}: {}",
                    fs::read_to_string(self.dir.path().join(name))
                        .unwrap_or_else(|error| format!("unavailable: {error}"))
                );
            }
        }
        for call in self.calls() {
            if let Some(pid) = call["pid"].as_i64() {
                // SAFETY: signalling a pid this test's stub recorded.
                unsafe {
                    libc::kill(pid as libc::pid_t, libc::SIGKILL);
                }
            }
        }
    }
}

/// A remote dashboard's two identity routes, answered from a thread.
fn fake_dashboard(machine_id: &str) -> u16 {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind fake dashboard");
    let port = listener.local_addr().expect("fake dashboard port").port();
    let hosts = json!({"hosts": [
        {"local": true, "machine_id": machine_id, "binary_version": "9.9.9",
         "protocol_fingerprint": "fixture"},
    ]})
    .to_string();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            let mut buf = [0u8; 2048];
            let n = stream.read(&mut buf).unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]);
            let (status, body) = if request.starts_with("GET /healthz ") {
                ("200 OK", String::new())
            } else if request.starts_with("GET /api/hosts?probe=false ") {
                ("200 OK", hosts.clone())
            } else {
                ("404 Not Found", String::new())
            };
            let _ = write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    port
}

fn config(stub: &Stub, remote_port: u16) -> TunnelConfig {
    TunnelConfig {
        ssh_program: stub.program(),
        remote_port,
        idle_timeout: Duration::from_secs(60),
        attach_timeout: Duration::from_millis(500),
        ready_timeout: Duration::from_secs(10),
        bind_timeout: Duration::from_secs(10),
        identity_timeout: Duration::from_secs(5),
    }
}

fn target(ssh: &str) -> HostTarget {
    HostTarget {
        name: "remote".to_string(),
        machine_id: MACHINE_ID.to_string(),
        ssh: ssh.to_string(),
    }
}

/// True until `pid` is gone and reaped: `kill(pid, 0)` still succeeds on a
/// zombie.
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

fn pids(stub: &Stub) -> Vec<i64> {
    stub.calls()
        .iter()
        .map(|call| call["pid"].as_i64().expect("stub pid"))
        .collect()
}

#[test]
fn concurrent_first_requests_share_one_attached_ssh() {
    let remote = fake_dashboard(MACHINE_ID);
    let stub = Stub::new(json!({"remote": {"attach": remote, "spawn": null}}));
    let tunnels = Arc::new(HostTunnels::new(config(&stub, remote)));
    let barrier = Arc::new(Barrier::new(8));
    let workers: Vec<_> = (0..8)
        .map(|_| {
            let tunnels = Arc::clone(&tunnels);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                tunnels
                    .acquire(&target("remote"), false)
                    .map(|lease| lease.local_port)
                    .map_err(|error| error.message)
            })
        })
        .collect();
    let ports: Vec<u16> = workers
        .into_iter()
        .map(|worker| {
            worker
                .join()
                .expect("acquire thread")
                .expect("acquire succeeds")
        })
        .collect();
    assert!(
        ports.windows(2).all(|pair| pair[0] == pair[1]),
        "every request shares one tunnel: {ports:?}"
    );
    let calls = stub.calls();
    assert_eq!(
        calls.len(),
        1,
        "single-flight establish runs ssh once: {calls:?}"
    );
    assert_eq!(calls[0]["mode"], "attach");
    assert_eq!(
        calls[0]["command"], "",
        "attach mode sends no remote command"
    );
    for option in ["BatchMode=yes", "ConnectTimeout=10"] {
        assert!(
            calls[0]["argv"]
                .as_array()
                .unwrap()
                .iter()
                .any(|arg| arg == option),
            "a server has no terminal to prompt on: {option} in {}",
            calls[0]["argv"]
        );
    }
    tunnels.shutdown_all();
    assert!(
        !alive(pids(&stub)[0]),
        "shutdown stops the attached forward"
    );
}

#[test]
fn a_dead_child_is_replaced_by_the_next_request() {
    let remote = fake_dashboard(MACHINE_ID);
    let stub = Stub::new(json!({"remote": {"attach": remote, "spawn": null}}));
    let tunnels = HostTunnels::new(config(&stub, remote));
    drop(
        tunnels
            .acquire(&target("remote"), false)
            .expect("first tunnel"),
    );
    let first = pids(&stub)[0];
    // SAFETY: the stub pid this test's tunnel started. `WNOWAIT` waits for
    // the exit without reaping, so the tunnel still owns that step.
    unsafe {
        libc::kill(first as libc::pid_t, libc::SIGKILL);
        let mut info: libc::siginfo_t = std::mem::zeroed();
        libc::waitid(
            libc::P_PID,
            first as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOWAIT,
        );
    }
    let lease = tunnels
        .acquire(&target("remote"), false)
        .expect("the next request re-establishes");
    let calls = pids(&stub);
    assert_eq!(calls.len(), 2, "one new ssh after the child died");
    assert!(!alive(first), "the dead child was reaped");
    assert!(alive(calls[1]));
    drop(lease);
    tunnels.shutdown_all();
    assert!(!alive(calls[1]));
}

#[test]
fn an_idle_tunnel_is_torn_down_but_not_under_an_open_lease() {
    let remote = fake_dashboard(MACHINE_ID);
    let stub = Stub::new(json!({"remote": {"attach": remote, "spawn": null}}));
    let idle = Duration::from_millis(300);
    let tunnels = HostTunnels::new(TunnelConfig {
        idle_timeout: idle,
        ..config(&stub, remote)
    });
    let lease = tunnels.acquire(&target("remote"), false).expect("tunnel");
    let pid = pids(&stub)[0];
    thread::sleep(idle * 3);
    assert!(
        alive(pid),
        "an in-flight lease (an open stream) keeps the tunnel"
    );
    drop(lease);
    wait_until(
        "the idle tunnel's child to be reaped",
        Duration::from_secs(10),
        || !alive(pid),
    );
    drop(
        tunnels
            .acquire(&target("remote"), false)
            .expect("a later request opens a new tunnel"),
    );
    assert_eq!(pids(&stub).len(), 2);
    tunnels.shutdown_all();
}

#[test]
fn shutdown_stops_every_child_and_refuses_new_tunnels() {
    let remote = fake_dashboard(MACHINE_ID);
    let other = fake_dashboard("hm_unit_other");
    let stub = Stub::new(json!({
        "remote": {"attach": remote, "spawn": null},
        "other": {"attach": other, "spawn": null},
    }));
    let tunnels = HostTunnels::new(config(&stub, remote));
    let held = tunnels.acquire(&target("remote"), false).expect("remote");
    drop(
        tunnels
            .acquire(
                &HostTarget {
                    name: "other".to_string(),
                    machine_id: "hm_unit_other".to_string(),
                    ssh: "other".to_string(),
                },
                false,
            )
            .expect("other"),
    );
    tunnels.shutdown_all();
    for pid in pids(&stub) {
        assert!(!alive(pid), "shutdown leaves no ssh child: {pid}");
    }
    drop(held);
    let refused = tunnels
        .acquire(&target("remote"), false)
        .err()
        .expect("no tunnel after shutdown");
    assert_eq!(refused.code, "unreachable_destination");
    assert_eq!(pids(&stub).len(), 2, "no ssh starts after shutdown");
}

/// Identity body bytes arrive faster than the probe's per-read socket timeout,
/// and never finish. The identity budget must still end establish.
#[test]
fn a_paced_identity_body_cannot_outlive_its_deadline() {
    let fragment = Duration::from_millis(100);
    assert!(
        fragment < crate::connect::PROBE_IO_TIMEOUT,
        "body fragments must arrive before one socket read times out"
    );
    let identity_budget = Duration::from_millis(500);
    let remote = pacing_identity(fragment);
    let stub = Stub::new(json!({"remote": {"attach": remote, "spawn": null}}));
    let tunnels = HostTunnels::new(TunnelConfig {
        identity_timeout: identity_budget,
        ..config(&stub, remote)
    });
    let (tx, rx) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let outcome = tunnels.acquire(&target("remote"), false).map(|_| ());
        let _ = tx.send(outcome);
    });
    // An unfinished body must return within this non-termination guard. A
    // second elapsed-time cap would also measure ssh startup and scheduling
    // delays on a loaded host, rather than the identity budget.
    let outcome = rx
        .recv_timeout(Duration::from_secs(10))
        .unwrap_or_else(|_| {
            panic!("a paced identity body held establish past the identity deadline")
        });
    let error = outcome.expect_err("an unfinished identity body must not open a tunnel");
    assert_eq!(error.code, "unreachable_destination", "{error:?}");
    let children = pids(&stub);
    assert_eq!(children.len(), 1, "attach succeeded and spawn did not run");
    assert!(!alive(children[0]), "the owned ssh child was not reaped");
}

/// `/healthz` answers at once. The identity body then trickles with no end.
fn pacing_identity(interval: Duration) -> u16 {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind pacing dashboard");
    let port = listener.local_addr().expect("pacing dashboard port").port();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let _ = stream.set_nodelay(true);
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            let _ = stream.set_write_timeout(Some(Duration::from_millis(300)));
            let mut buf = [0u8; 2048];
            let n = stream.read(&mut buf).unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]);
            if request.starts_with("GET /healthz ") {
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                );
                continue;
            }
            if !request.starts_with("GET /api/hosts?probe=false ") {
                continue;
            }
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 1000000\r\nConnection: close\r\n\r\n",
            );
            for _ in 0..500 {
                if stream.write_all(b"{").is_err() {
                    break;
                }
                thread::sleep(interval);
            }
        }
    });
    port
}
