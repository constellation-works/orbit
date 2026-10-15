//! ORB-14057: the attach budget starts when the local forward accepts a TCP
//! connection, not when `ssh` is spawned.
//!
//! Admitted as a unit test (fault injection). `connect` cannot host it: that
//! entry point blocks on signals and may open a browser. Host forwarding uses
//! the same probe, but fixes its attach/ready budgets and hides the direct
//! probe result; the private `TunnelSpec`/`ProbeLimit` seams inject the short
//! budgets below. The stub is a real executable with ssh's `-L` argv contract.

use std::fs;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpListener};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use crate::connect::{self, healthz_ok};
use crate::ssh_tunnel::{self, ProbeLimit, TunnelOrigin, TunnelSpec, Unattended};

/// Longer than [`ATTACH_BUDGET`], so a budget that starts at process spawn
/// expires while the listener is still down.
const BIND_DELAY: Duration = Duration::from_millis(2000);
const ATTACH_BUDGET: Duration = Duration::from_millis(500);

const STUB_SSH: &str = r#"#!/usr/bin/env python3
import json, os, socket, subprocess, sys, threading, time
from pathlib import Path

def phase(stage, **details):
    path = Path(sys.argv[0]).resolve().parent / "phases.jsonl"
    event = {"stage": stage, "pid": os.getpid(), "parent": os.getppid(),
             "monotonic_ns": time.monotonic_ns(), **details}
    fd = os.open(path, os.O_WRONLY | os.O_APPEND | os.O_CREAT, 0o600)
    try:
        os.write(fd, (json.dumps(event) + "\n").encode())
    finally:
        os.close(fd)

def main():
    phase("child_started")
    cfg = Path(sys.argv[0]).resolve().parent / "ssh.cfg"
    delay_line, count_path = cfg.read_text().splitlines()[:2]
    fd = os.open(count_path, os.O_WRONLY | os.O_APPEND | os.O_CREAT, 0o644)
    os.write(fd, (" ".join(sys.argv[1:]) + "\n").encode())
    os.close(fd)
    pid_fd = os.open(count_path + ".pids", os.O_WRONLY | os.O_APPEND | os.O_CREAT, 0o644)
    os.write(pid_fd, (str(os.getpid()) + "\n").encode())
    os.close(pid_fd)
    local_port, remote_port = parse_forward(sys.argv[1:])
    time.sleep(float(delay_line))
    listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    phase("bind_start", port=local_port)
    try:
        listener.bind(("127.0.0.1", local_port))
    except OSError as error:
        phase("bind_failed", port=local_port, errno=error.errno)
        raise
    listener.listen(32)
    phase("bound", port=local_port)
    local_command(sys.argv[1:])
    phase("proxy_ready")
    while True:
        client, _ = listener.accept()
        phase("proxy_accepted")
        threading.Thread(
            target=proxy, args=(client, remote_port), daemon=True
        ).start()

def local_command(argv):
    # ssh runs -o LocalCommand once its forwards are bound.
    opts = dict(argv[i + 1].split("=", 1) for i, a in enumerate(argv) if a == "-o")
    if opts.get("PermitLocalCommand") == "yes" and "LocalCommand" in opts:
        shell = os.environ.get("SHELL") or "/bin/sh"
        phase("local_command_start", shell=shell)
        result = subprocess.run([shell, "-c", opts["LocalCommand"].replace("%%", "%")])
        phase("local_command_end", status=result.returncode)

def parse_forward(argv):
    for i, arg in enumerate(argv):
        if arg == "-L" and i + 1 < len(argv):
            parts = argv[i + 1].split(":")
            if len(parts) == 4:
                return int(parts[1]), int(parts[3])
    sys.stderr.write("stub ssh: expected -L host:local:host:remote\n")
    sys.exit(255)

def proxy(client, remote_port):
    try:
        remote = socket.create_connection(("127.0.0.1", remote_port), timeout=2)
    except OSError as error:
        phase("remote_failed", errno=error.errno)
        client.close()
        return
    phase("remote_connected", port=remote_port)
    def pump(src, dst):
        first = src is client
        try:
            while True:
                data = src.recv(65536)
                if not data:
                    break
                if first:
                    phase("probe_request", request=data.split(b"\r\n", 1)[0].decode("ascii", "replace"))
                    first = False
                dst.sendall(data)
        except OSError:
            pass
        try:
            dst.shutdown(socket.SHUT_WR)
        except OSError:
            pass
    left = threading.Thread(target=pump, args=(client, remote), daemon=True)
    right = threading.Thread(target=pump, args=(remote, client), daemon=True)
    left.start()
    right.start()
    left.join()
    right.join()
    client.close()
    remote.close()

if __name__ == "__main__":
    main()
"#;

/// Snapshot phases and stop logged children before cleaning up a failed fixture.
struct StubDir(tempfile::TempDir);

impl StubDir {
    fn new() -> Self {
        Self(tempfile::tempdir().expect("temp dir"))
    }

    fn path(&self) -> &Path {
        self.0.path()
    }
}

impl Drop for StubDir {
    fn drop(&mut self) {
        if thread::panicking() {
            for name in ["spawns.log", "spawns.log.pids", "phases.jsonl"] {
                eprintln!(
                    "SSH fixture {name}: {}",
                    fs::read_to_string(self.path().join(name))
                        .unwrap_or_else(|error| format!("unavailable: {error}"))
                );
            }
            for pid in child_pids(&self.path().join("spawns.log")) {
                // SAFETY: this fixture recorded the child it started. Its
                // establish thread owns reaping, just as on a normal error.
                unsafe {
                    libc::kill(pid, libc::SIGKILL);
                }
            }
        }
    }
}

#[test]
fn delayed_forward_bind_attaches_without_a_second_ssh() {
    assert!(
        BIND_DELAY > ATTACH_BUDGET,
        "the stub must bind only after the attach budget would have expired"
    );
    let python = Command::new("python3")
        .arg("--version")
        .output()
        .expect("python3 is required to run the ssh stub");
    assert!(
        python.status.success(),
        "python3 --version failed: {}",
        String::from_utf8_lossy(&python.stderr)
    );

    let dir = StubDir::new();
    let count_path = dir.path().join("spawns.log");
    let stub = write_stub(dir.path(), BIND_DELAY, &count_path);

    let health = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind health server");
    let health_port = health.local_addr().expect("health port").port();
    let hits = Arc::new(AtomicUsize::new(0));
    let hits_bg = Arc::clone(&hits);
    thread::spawn(move || serve_healthz(health, hits_bg));

    let local_port = ssh_tunnel::ephemeral_port().expect("local forward port");
    let spec = TunnelSpec {
        ssh_host: "stub-host".to_string(),
        local_port,
        remote_port: health_port,
        remote_command: format!("orbit web serve --no-open --port {health_port}"),
        remote_description: "orbit web serve".to_string(),
        readiness_target: format!("http://127.0.0.1:{local_port}/healthz"),
        attach_timeout: ATTACH_BUDGET,
        // Long enough that the old bug (a second ssh after the budget) still
        // reaches the healthy server and returns Spawned instead of hanging.
        ready_timeout: Duration::from_secs(8),
        ssh_program: stub.display().to_string(),
        unattended: None,
    };

    let (tx, rx) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let outcome = ssh_tunnel::establish(&spec, |limit| healthz_ok(local_port, limit));
        let outcome = outcome.map(|(tunnel, origin)| {
            drop(tunnel);
            origin
        });
        let _ = tx.send(outcome);
    });
    let outcome = rx
        .recv_timeout(Duration::from_secs(15))
        .unwrap_or_else(|_| {
            panic!("establish hung; attach must return once the delayed forward binds")
        });
    let origin = outcome.expect("attach to the server behind the late forward");

    assert_eq!(
        origin,
        TunnelOrigin::Attached,
        "ORB-14057: a healthy server behind a forward that binds after the \
         attach timeout must attach, not spawn a remote serve"
    );
    let log = fs::read_to_string(&count_path).unwrap_or_default();
    let spawns: Vec<&str> = log.lines().filter(|line| !line.is_empty()).collect();
    assert_eq!(
        spawns.len(),
        1,
        "ORB-14057: ssh must be spawned once when auth outlasts the attach \
         timeout; got {log:?}"
    );
    assert!(
        spawns[0].split_whitespace().next() == Some("-N"),
        "ORB-14057: the single spawn must be the bare probe forward, got {}",
        spawns[0]
    );
    assert!(
        hits.load(Ordering::SeqCst) >= 1,
        "ORB-14057: /healthz must reach the server behind the forward"
    );
}

fn write_stub(dir: &Path, delay: Duration, count_path: &Path) -> PathBuf {
    let script = dir.join("ssh");
    fs::write(&script, STUB_SSH).expect("write ssh stub");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("chmod ssh stub");
    fs::write(
        dir.join("ssh.cfg"),
        format!("{}\n{}\n", delay.as_secs_f64(), count_path.display()),
    )
    .expect("write ssh stub config");
    script
}

fn serve_healthz(listener: TcpListener, hits: Arc<AtomicUsize>) {
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else {
            break;
        };
        let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
        let mut buf = [0u8; 1024];
        let n = stream.read(&mut buf).unwrap_or(0);
        if n >= 12 && &buf[..12] == b"GET /healthz" {
            hits.fetch_add(1, Ordering::SeqCst);
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        }
    }
}

/// Shorter than [`connect::PROBE_IO_TIMEOUT`], so a per-read socket timeout
/// cannot end a status line this peer keeps feeding.
const FRAGMENT: Duration = Duration::from_millis(150);

const ATTACH_SLICE: Duration = Duration::from_millis(600);
const READY_SLICE: Duration = Duration::from_millis(600);

#[test]
fn paced_status_fragments_cannot_outlive_the_readiness_budget() {
    assert!(
        FRAGMENT < connect::PROBE_IO_TIMEOUT,
        "fragments must arrive before one socket read times out"
    );
    let dir = StubDir::new();
    let count_path = dir.path().join("spawns.log");
    let stub = write_stub(dir.path(), Duration::from_millis(50), &count_path);
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind paced peer");
    let remote_port = listener.local_addr().expect("paced port").port();
    thread::spawn(move || serve_paced(listener, FRAGMENT, None));
    let local_port = ssh_tunnel::ephemeral_port().expect("local forward port");
    let spec = spec_for(
        &stub,
        local_port,
        remote_port,
        ATTACH_SLICE,
        READY_SLICE,
        None,
    );
    let started = Instant::now();
    let outcome = run_establish(spec, local_port, Duration::from_secs(20));
    let elapsed = started.elapsed();
    let budget = ATTACH_SLICE + READY_SLICE + Duration::from_secs(6);
    assert!(
        elapsed < budget,
        "a paced status line extended establish to {elapsed:?}; budget {budget:?}"
    );
    let error = outcome.expect_err("a status line with no newline is not healthy");
    assert!(
        error.to_string().contains("timed out"),
        "the readiness budget must refuse the paced peer, got {error}"
    );
    assert_reaped(&count_path);
}

#[test]
fn cancel_during_a_paced_status_reaps_the_ssh_child() {
    let dir = StubDir::new();
    let count_path = dir.path().join("spawns.log");
    let stub = write_stub(dir.path(), Duration::from_millis(50), &count_path);
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind paced peer");
    let remote_port = listener.local_addr().expect("paced port").port();
    let (started_tx, started_rx) = mpsc::channel();
    thread::spawn(move || serve_paced(listener, FRAGMENT, Some(started_tx)));
    let local_port = ssh_tunnel::ephemeral_port().expect("local forward port");
    let cancel = Arc::new(AtomicBool::new(false));
    let spec = spec_for(
        &stub,
        local_port,
        remote_port,
        Duration::from_secs(30),
        Duration::from_secs(30),
        Some(Arc::clone(&cancel)),
    );
    let (tx, rx) = mpsc::sync_channel(1);
    let started = Instant::now();
    thread::spawn(move || {
        let outcome = ssh_tunnel::establish(&spec, |limit| healthz_ok(local_port, limit));
        let outcome = outcome.map(|(tunnel, origin)| {
            drop(tunnel);
            origin
        });
        let _ = tx.send(outcome);
    });
    started_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("the paced peer must accept a probe before cancel");
    cancel.store(true, Ordering::Relaxed);
    let outcome = rx
        .recv_timeout(Duration::from_secs(10))
        .unwrap_or_else(|_| panic!("cancel during a paced status line did not end establish"));
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(5),
        "cancel left establish running for {elapsed:?}"
    );
    let error = outcome.expect_err("cancel must not attach");
    assert!(
        error.to_string().contains("shutting down"),
        "cancel must stop establish, got {error}"
    );
    assert_reaped(&count_path);
}

#[test]
fn oversized_status_line_is_refused_without_draining_the_flood() {
    // Direct probe: a continuous flood has no newline and no idle gap, so an
    // uncapped read runs until the 5s deadline. The cap must return first.
    // Socket buffers hide how many bytes the peer accepted, so the clock is
    // the observation.
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind flood peer");
    let port = listener.local_addr().expect("flood port").port();
    let accepted = Arc::new(AtomicUsize::new(0));
    let accepted_bg = Arc::clone(&accepted);
    thread::spawn(move || serve_flood(listener, accepted_bg));
    let (tx, rx) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let started = Instant::now();
        let ok = healthz_ok(
            port,
            ProbeLimit {
                deadline: Instant::now() + Duration::from_secs(5),
                cancel: None,
            },
        );
        let _ = tx.send((ok, started.elapsed()));
    });
    let (ok, elapsed) = rx.recv_timeout(Duration::from_secs(2)).unwrap_or_else(|_| {
        panic!(
            "an oversized status line held the probe; the {} byte cap must return before the deadline",
            connect::PROBE_MAX_STATUS_LINE_BYTES
        )
    });
    assert!(!ok, "a status line with no newline is not healthy");
    assert!(
        elapsed < Duration::from_secs(1),
        "capped status read took {elapsed:?}"
    );
    assert!(
        accepted.load(Ordering::SeqCst) >= 1,
        "the flood peer never saw a probe"
    );

    let dir = StubDir::new();
    let count_path = dir.path().join("spawns.log");
    let stub = write_stub(dir.path(), Duration::from_millis(50), &count_path);
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind tunneled flood");
    let remote_port = listener.local_addr().expect("tunneled flood port").port();
    thread::spawn(move || serve_flood(listener, Arc::new(AtomicUsize::new(0))));
    let local_port = ssh_tunnel::ephemeral_port().expect("local forward port");
    let spec = spec_for(
        &stub,
        local_port,
        remote_port,
        ATTACH_SLICE,
        READY_SLICE,
        None,
    );
    let started = Instant::now();
    let outcome = run_establish(spec, local_port, Duration::from_secs(20));
    let elapsed = started.elapsed();
    let budget = ATTACH_SLICE + READY_SLICE + Duration::from_secs(6);
    assert!(
        elapsed < budget,
        "an oversized status line extended establish to {elapsed:?}; budget {budget:?}"
    );
    outcome.expect_err("an oversized status line is not healthy");
    assert_reaped(&count_path);
}

fn spec_for(
    stub: &Path,
    local_port: u16,
    remote_port: u16,
    attach_timeout: Duration,
    ready_timeout: Duration,
    cancel: Option<Arc<AtomicBool>>,
) -> TunnelSpec {
    TunnelSpec {
        ssh_host: "stub-host".to_string(),
        local_port,
        remote_port,
        remote_command: format!("orbit web serve --no-open --port {remote_port}"),
        remote_description: "orbit web serve".to_string(),
        readiness_target: format!("http://127.0.0.1:{local_port}/healthz"),
        attach_timeout,
        ready_timeout,
        ssh_program: stub.display().to_string(),
        unattended: cancel.map(|cancel| Unattended {
            ssh_options: Vec::new(),
            bind_timeout: Duration::from_secs(5),
            cancel,
        }),
    }
}

fn run_establish(
    spec: TunnelSpec,
    local_port: u16,
    wait: Duration,
) -> Result<TunnelOrigin, orbit_core::OrbitError> {
    let (tx, rx) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let outcome = ssh_tunnel::establish(&spec, |limit| healthz_ok(local_port, limit));
        let outcome = outcome.map(|(tunnel, origin)| {
            drop(tunnel);
            origin
        });
        let _ = tx.send(outcome);
    });
    rx.recv_timeout(wait)
        .unwrap_or_else(|_| {
            panic!("establish hung; a probe must return within the readiness budget")
        })
        .map_err(orbit_core::OrbitError::from)
}

fn child_pids(count_path: &Path) -> Vec<i32> {
    let path = format!("{}.pids", count_path.display());
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| line.parse().ok())
        .collect()
}

fn assert_reaped(count_path: &Path) {
    let pids = child_pids(count_path);
    assert!(!pids.is_empty(), "the ssh stub recorded no child to reap");
    let deadline = Instant::now() + Duration::from_secs(3);
    for pid in pids {
        // SAFETY: signal 0 only checks whether this test's ssh child still exists.
        while unsafe { libc::kill(pid, 0) } == 0 {
            assert!(
                Instant::now() < deadline,
                "owned ssh child {pid} was not reaped"
            );
            thread::sleep(Duration::from_millis(20));
        }
    }
}

/// Write a status line with no newline, one byte at a time.
fn serve_paced(listener: TcpListener, interval: Duration, started: Option<mpsc::Sender<()>>) {
    let mut signaled = false;
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else {
            break;
        };
        let _ = stream.set_nodelay(true);
        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
        let _ = stream.set_write_timeout(Some(Duration::from_millis(300)));
        let mut buf = [0u8; 2048];
        let _ = stream.read(&mut buf);
        if stream.write_all(b"H").is_err() {
            continue;
        }
        if !signaled {
            if let Some(start) = &started {
                let _ = start.send(());
            }
            signaled = true;
        }
        for _ in 0..400 {
            thread::sleep(interval);
            if stream.write_all(b"H").is_err() {
                break;
            }
        }
    }
}

/// Offer megabytes of status-line bytes with no newline and no idle gap.
fn serve_flood(listener: TcpListener, accepted: Arc<AtomicUsize>) {
    let chunk = [b'A'; 4096];
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else {
            break;
        };
        let _ = stream.set_nodelay(true);
        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
        let _ = stream.set_write_timeout(Some(Duration::from_millis(200)));
        let mut buf = [0u8; 2048];
        let _ = stream.read(&mut buf);
        accepted.fetch_add(1, Ordering::SeqCst);
        let mut sent = 0usize;
        while sent < 8 * 1024 * 1024 {
            match stream.write(&chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => sent += n,
            }
        }
    }
}

/// One finite loopback peer that answers a single request with `response` and
/// then ends. It accepts exactly one connection and has no loop, so the thread
/// finishes deterministically once the probe has read.
fn one_shot_peer(response: &'static [u8]) -> (u16, thread::JoinHandle<()>) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind one-shot peer");
    let port = listener.local_addr().expect("one-shot peer port").port();
    let handle = thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
        let _ = stream.set_write_timeout(Some(Duration::from_millis(500)));
        let mut buf = [0u8; 1024];
        let _ = stream.read(&mut buf);
        let _ = stream.write_all(response);
    });
    (port, handle)
}

#[test]
fn probes_verify_status_code_over_finite_peer() {
    // Each row is one response: whether `/healthz` is healthy, and the body
    // `http_get_ok_body` returns for it. A non-200 status is refused even when
    // its reason phrase contains "200", and malformed status lines are refused.
    let cases: &[(&[u8], bool, Option<&str>)] = &[
        (
            b"HTTP/1.1 503 upstream expected 200 but failed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            false,
            None,
        ),
        (
            b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello",
            true,
            Some("hello"),
        ),
        (
            b"HTTP/1.1 200\r\nContent-Length: 4\r\nConnection: close\r\n\r\nbare",
            true,
            Some("bare"),
        ),
        (
            b"HTTP/1.0 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello",
            true,
            Some("hello"),
        ),
        (
            b"HTTP/1.1 +200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            false,
            None,
        ),
        (
            b"HTTP/1.1 0200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            false,
            None,
        ),
        (
            b"HTTP/1.invalid 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            false,
            None,
        ),
        (
            b"HTTP/1.1 not_a_number OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            false,
            None,
        ),
        (
            b"HTTP/1.1\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            false,
            None,
        ),
        (
            b"HTTP/2.0 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            false,
            None,
        ),
    ];

    for &(response, expected_health, expected_body) in cases {
        let (port, handle) = one_shot_peer(response);
        let limit = ProbeLimit {
            deadline: Instant::now() + Duration::from_secs(5),
            cancel: None,
        };
        assert_eq!(
            healthz_ok(port, limit),
            expected_health,
            "healthz_ok verdict for response: {:?}",
            String::from_utf8_lossy(response)
        );
        handle.join().expect("peer join");

        let (port, handle) = one_shot_peer(response);
        assert_eq!(
            connect::http_get_ok_body(port, "/api/routines").as_deref(),
            expected_body,
            "http_get_ok_body body for response: {:?}",
            String::from_utf8_lossy(response)
        );
        handle.join().expect("peer join");
    }
}
