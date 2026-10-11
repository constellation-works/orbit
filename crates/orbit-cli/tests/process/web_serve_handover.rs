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

use std::hash::{BuildHasher, RandomState};
use std::net::SocketAddr;
use std::path::Path;

use crate::generation_fixture;
use std::process::{Command, Stdio};

use crate::bound_server::{Announcement, BoundServer};
use crate::web_fixture_io::{DeadlineStream, OPERATION_TIMEOUT, http_get};
use std::time::{Duration, Instant};

use orbit_common::fs::generation::executable_generation;
use orbit_common::test_env;
use tempfile::tempdir;

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
        DeadlineStream::connect(port, OPERATION_TIMEOUT).is_err(),
        "the handed-over dashboard must release its listening port"
    );
}

fn exercise_handover(after_handover: impl FnOnce(BoundServer, u32, u16)) {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let install = temp.path().join("installation");
    std::fs::create_dir_all(&home).expect("create home");
    std::fs::create_dir_all(&install).expect("create installation");
    let installed = install.join("orbit");
    std::fs::copy(env!("CARGO_BIN_EXE_orbit"), &installed).expect("install old executable");

    let (mut server, address) = start_dashboard(&installed, &home);
    let port = address.port();
    let pid = server.id();
    assert!(http_get(port, "/healthz").contains("ok"));

    let candidate = temp.path().join("candidate");
    generation_fixture::distinct_copy(Path::new(env!("CARGO_BIN_EXE_orbit")), &candidate);
    let new_digest = executable_generation(&candidate).expect("candidate digest");
    generation_fixture::install_over(&candidate, &installed);

    test_env::wait_until("handover", || {
        generation_fixture::running_digest(&home.join(".orbit"), pid).as_deref()
            == Some(&new_digest)
    });
    assert!(
        matches!(server.try_wait(), Ok(None)),
        "the dashboard must exec in place, not exit"
    );
    let replacement_address = server
        .wait_for_address(test_env::FIXTURE_STEP_DEADLINE)
        .expect("replacement dashboard bound");
    assert_eq!(
        replacement_address, address,
        "handover must keep its assigned address"
    );
    assert!(http_get(port, "/healthz").contains("ok"));

    after_handover(server, pid, port);
}

// Handover reuses argv, so this fixture needs a nonzero requested port.
// Let the child itself claim each candidate; no temporary listener is dropped
// to select a port. A competing bind is a retry, never successful readiness.
fn start_dashboard(program: &Path, home: &Path) -> (BoundServer, SocketAddr) {
    let offset = (RandomState::new().hash_one(home) % 16384) as u16;
    for attempt in 0..16 {
        let port = 49152 + (offset + attempt) % 16384;
        let mut server = spawn_dashboard(program, home, port);
        match server.wait_for_address(test_env::FIXTURE_STEP_DEADLINE) {
            Ok(address) => {
                assert_eq!(address, SocketAddr::from(([127, 0, 0, 1], port)));
                return (server, address);
            }
            Err(error) => {
                let diagnostic = error.to_string();
                assert!(
                    diagnostic.contains(&format!("bind 127.0.0.1:{port}:"))
                        && diagnostic.contains("Address already in use"),
                    "dashboard startup failed: {diagnostic}"
                );
            }
        }
    }
    panic!("dashboard could not bind any candidate address");
}

fn spawn_dashboard(program: &Path, home: &Path, port: u16) -> BoundServer {
    let mut command = Command::new(program);
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .env("HOME", home)
        .env("USERPROFILE", home)
        .args(["web", "serve", "--port", &port.to_string(), "--no-open"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // The copy was just written. Retry only ExecutableFileBusy; every other
    // spawn error still fails on the first attempt.
    generation_fixture::launch(|| command.spawn())
        .map(|child| BoundServer::new(child, Announcement::Dashboard))
        .expect("spawn orbit web serve")
}
