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
#![cfg(any(target_os = "linux", target_os = "macos"))]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;

use crate::generation_fixture;
use std::process::{Command, Stdio};

use crate::child_guard::ChildGuard;
use std::time::{Duration, Instant};

use orbit_common::fs::generation::executable_generation;
use orbit_common::test_env;
use tempfile::tempdir;

const HANDOVER_DEADLINE: Duration = Duration::from_secs(60);

#[test]
fn a_replaced_dashboard_execs_the_installed_executable_and_keeps_serving() {
    exercise_handover(|mut server, pid, _| {
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
    });
}

#[test]
fn a_handed_over_dashboard_is_reaped_when_an_assertion_panics() {
    let mut spawned = None;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        exercise_handover(|server, pid, port| {
            spawned = Some((pid, port));
            assert_eq!(server.id(), 0, "forced assertion failure after handover");
        });
    }));
    let (pid, port) = spawned.expect("dashboard completed its handover before the forced panic");
    assert!(
        result.is_err(),
        "the assertion must unwind through the guard"
    );
    // SAFETY: probe the PID retained before unwinding; do not send a signal.
    assert_eq!(unsafe { libc::kill(pid as libc::pid_t, 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
    assert!(
        TcpStream::connect(("127.0.0.1", port)).is_err(),
        "the handed-over dashboard must release its listening port"
    );
}

fn exercise_handover(after_handover: impl FnOnce(ChildGuard, u32, u16)) {
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
    generation_fixture::distinct_copy(Path::new(env!("CARGO_BIN_EXE_orbit")), &candidate);
    let new_digest = executable_generation(&candidate).expect("candidate digest");
    let staged = install.join("orbit.staged");
    std::fs::copy(&candidate, &staged).expect("stage replacement");
    std::fs::rename(&staged, &installed).expect("replace installation");

    wait_until(
        || {
            generation_fixture::running_digest(&home.join(".orbit"), pid).as_deref()
                == Some(&new_digest)
        },
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

    after_handover(server, pid, port);
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

fn spawn_dashboard(program: &Path, home: &Path, port: u16) -> ChildGuard {
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
    generation_fixture::launch(|| command.spawn())
        .map(ChildGuard::new)
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
