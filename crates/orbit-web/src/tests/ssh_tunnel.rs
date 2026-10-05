//! ORB-14057: the attach budget starts when the local forward accepts a TCP
//! connection, not when `ssh` is spawned.
//!
//! Admitted as a unit test (fault injection). `connect` cannot host it: that
//! entry point blocks on signals and may open a browser, and `establish` is
//! crate-private. The stub is a real executable with ssh's `-L` argv contract.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use crate::ssh_tunnel::{self, TunnelOrigin, TunnelSpec};

/// Longer than [`ATTACH_BUDGET`], so a budget that starts at process spawn
/// expires while the listener is still down.
const BIND_DELAY: Duration = Duration::from_millis(2000);
const ATTACH_BUDGET: Duration = Duration::from_millis(500);

const STUB_SSH: &str = r#"#!/usr/bin/env python3
import os, socket, sys, threading, time
from pathlib import Path

def main():
    cfg = Path(sys.argv[0]).resolve().parent / "ssh.cfg"
    delay_line, count_path = cfg.read_text().splitlines()[:2]
    fd = os.open(count_path, os.O_WRONLY | os.O_APPEND | os.O_CREAT, 0o644)
    os.write(fd, (" ".join(sys.argv[1:]) + "\n").encode())
    os.close(fd)
    local_port, remote_port = parse_forward(sys.argv[1:])
    time.sleep(float(delay_line))
    listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    listener.bind(("127.0.0.1", local_port))
    listener.listen(32)
    while True:
        client, _ = listener.accept()
        threading.Thread(
            target=proxy, args=(client, remote_port), daemon=True
        ).start()

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
    except OSError:
        client.close()
        return
    def pump(src, dst):
        try:
            while True:
                data = src.recv(65536)
                if not data:
                    break
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

    let dir = tempfile::tempdir().expect("temp dir");
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
    };

    let (tx, rx) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let outcome = ssh_tunnel::establish(&spec, || healthz_ok(local_port));
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

fn healthz_ok(port: u16) -> bool {
    let addr = std::net::SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_millis(500)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(500)));
    if stream
        .write_all(b"GET /healthz HTTP/1.0\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .is_err()
    {
        return false;
    }
    let mut status = String::new();
    if BufReader::new(stream).read_line(&mut status).is_err() {
        return false;
    }
    status.starts_with("HTTP/1.") && status.contains(" 200 ")
}
