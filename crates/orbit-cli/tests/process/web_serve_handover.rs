//! `orbit web serve` hands over to a replacement executable instead of
//! pinning the generation it started under.
//!
//! The dashboard is spawned from an installed copy of the binary. Replacing
//! that copy the way an installer does (write beside it, rename over it) must
//! make the idle dashboard drain, exec the installed executable in place, and
//! serve again on the same address.

#![allow(missing_docs)]
// Integration fixtures use expect/unwrap for concise failure diagnostics.
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![cfg(target_os = "linux")]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use orbit_common::fs::generation::executable_generation;
use orbit_common::test_env;
use tempfile::tempdir;

const HANDOVER_DEADLINE: Duration = Duration::from_secs(60);

#[test]
fn a_replaced_dashboard_execs_the_installed_executable_and_keeps_serving() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let install = temp.path().join("installation");
    std::fs::create_dir_all(&home).expect("create home");
    std::fs::create_dir_all(&install).expect("create installation");
    let installed = install.join("orbit");
    std::fs::copy(env!("CARGO_BIN_EXE_orbit"), &installed).expect("install old executable");

    let port = free_port();
    let mut server = spawn_dashboard(&installed, &home, port);
    let pid = server.id();
    wait_until(
        || TcpStream::connect(("127.0.0.1", port)).is_ok(),
        "listening",
    );
    assert!(http_get(port, "/healthz").contains("ok"));

    let candidate = temp.path().join("candidate");
    std::fs::copy(env!("CARGO_BIN_EXE_orbit"), &candidate).expect("candidate copy");
    std::fs::OpenOptions::new()
        .append(true)
        .open(&candidate)
        .expect("open candidate")
        .write_all(b"\ndashboard-handover-candidate\n")
        .expect("distinct executable");
    let new_digest = executable_generation(&candidate).expect("candidate digest");
    let staged = install.join("orbit.staged");
    std::fs::copy(&candidate, &staged).expect("stage replacement");
    std::fs::rename(&staged, &installed).expect("replace installation");

    wait_until(
        || running_digest(pid).as_deref() == Some(&new_digest),
        "handover",
    );
    assert!(
        matches!(server.try_wait(), Ok(None)),
        "the dashboard must exec in place, not exit"
    );
    wait_until(
        || TcpStream::connect(("127.0.0.1", port)).is_ok(),
        "listening after the handover",
    );
    assert!(http_get(port, "/healthz").contains("ok"));

    // Safety: SIGTERM to this test's own child, as a service manager would.
    assert_eq!(unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) }, 0);
    let deadline = Instant::now() + Duration::from_secs(20);
    while !matches!(server.try_wait(), Ok(Some(_))) {
        if Instant::now() >= deadline {
            let _ = server.kill();
            panic!("the handed-over dashboard did not stop on SIGTERM");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn running_digest(pid: u32) -> Option<String> {
    executable_generation(&PathBuf::from(format!("/proc/{pid}/exe"))).ok()
}

fn wait_until(mut ready: impl FnMut() -> bool, what: &str) {
    let deadline = Instant::now() + HANDOVER_DEADLINE;
    while !ready() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn free_port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0))
        .expect("bind ephemeral port")
        .local_addr()
        .expect("local addr")
        .port()
}

fn spawn_dashboard(program: &Path, home: &Path, port: u16) -> Child {
    let mut command = Command::new(program);
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .env("HOME", home)
        .env("USERPROFILE", home)
        .args(["web", "serve", "--port", &port.to_string(), "--no-open"])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // The copy was just written. Retry only ExecutableFileBusy; every other
    // spawn error still fails on the first attempt.
    orbit_common::test_process::retry_executable_busy(|| command.spawn())
        .expect("spawn orbit web serve")
}

fn http_get(port: u16, path: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    stream
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .expect("write request");
    let mut response = String::new();
    stream.read_to_string(&mut response).expect("read response");
    response
}
