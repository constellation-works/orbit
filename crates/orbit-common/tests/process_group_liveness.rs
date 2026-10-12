//! The process-group liveness probe at its public boundary: pid 0 names the
//! caller's own group to `killpg`, so the probe must never report it as alive.
//! Native Linux scanning must also distinguish an unreaped zombie-only group.
//! Owned-child cleanup and signal guards are exercised through the public API.
#![cfg(unix)]
#![allow(missing_docs, clippy::expect_used, clippy::unwrap_used)]

orbit_common::isolate_test_process!();

use orbit_common::process::identity::{KernelLiveness, probe_process_group_liveness};

#[test]
fn pid_zero_is_not_probed_as_the_callers_group() {
    // Safety: `getpgrp` only reads the calling process's group id.
    let own_group = unsafe { libc::getpgrp() };
    assert_eq!(
        probe_process_group_liveness(own_group),
        KernelLiveness::Alive,
        "the test process must be a live member of its own group, or pid 0 proves nothing"
    );
    assert_eq!(
        probe_process_group_liveness(0),
        KernelLiveness::Exited,
        "pid 0 must not probe as the caller's group"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn unreaped_linux_zombie_group_is_exited_despite_signal_presence() {
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    use orbit_common::process::identity::linux_process_state;

    struct ReapingChild(Child);
    impl Drop for ReapingChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    let mut child = ReapingChild(
        Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .expect("spawn child in its own group"),
    );
    let pid = child.0.id();
    let pgid = pid as libc::pid_t;
    let deadline = Instant::now() + Duration::from_secs(3);
    while linux_process_state(pid) != Some(('Z', pgid)) {
        assert!(
            Instant::now() < deadline,
            "child must become an unreaped zombie"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    // Safety: signal 0 checks presence without signalling any process.
    assert_eq!(
        unsafe { libc::kill(-pgid, 0) },
        0,
        "unreaped group still exists"
    );
    assert_eq!(probe_process_group_liveness(pgid), KernelLiveness::Exited);
    child.0.wait().expect("reap zombie");
}

use std::io::BufRead;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use orbit_common::process::{OwnedChild, run_bounded};

struct GroupPeer(Child);

impl Drop for GroupPeer {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn owned_group(ignore_term: bool) -> (OwnedChild, GroupPeer) {
    let leader = OwnedChild::from_spawned(
        Command::new("/bin/sleep")
            .arg("60")
            .process_group(0)
            .spawn()
            .expect("spawn group leader"),
    );
    let mut peer = GroupPeer(
        Command::new("/bin/sh")
            .args([
                "-c",
                if ignore_term {
                    "trap '' TERM; echo ready; exec sleep 60"
                } else {
                    "echo ready; exec sleep 60"
                },
            ])
            .stdout(Stdio::piped())
            .process_group(leader.pid() as i32)
            .spawn()
            .expect("spawn independently waitable group peer"),
    );
    let mut ready = String::new();
    std::io::BufReader::new(peer.0.stdout.take().expect("readiness pipe"))
        .read_line(&mut ready)
        .expect("peer readiness");
    assert_eq!(ready, "ready\n");
    (leader, peer)
}

#[test]
fn owned_cleanup_kills_group_members_and_reaps_the_leader() {
    for action in ["drop", "unwind", "kill", "terminate"] {
        let (mut leader, mut peer) = owned_group(true);
        let pid = leader.pid();
        match action {
            "drop" => drop(leader),
            "unwind" => {
                let panic = std::panic::catch_unwind(move || {
                    let _leader = leader;
                    panic!("exercise cleanup during unwinding");
                });
                assert!(panic.is_err());
            }
            "kill" => leader.kill_and_reap(),
            "terminate" => leader
                .terminate(Duration::from_millis(100))
                .expect("TERM grace and KILL escalation"),
            _ => unreachable!(),
        }
        // SAFETY: this PID belongs to our leader. ECHILD proves it was reaped.
        let waited = unsafe { libc::waitpid(pid as i32, std::ptr::null_mut(), libc::WNOHANG) };
        let error = std::io::Error::last_os_error().raw_os_error();
        assert_eq!((waited, error), (-1, Some(libc::ECHILD)), "{action}");
        let deadline = Instant::now() + Duration::from_secs(3);
        let status = loop {
            if let Some(status) = peer.0.try_wait().expect("wait for peer") {
                break status;
            }
            assert!(Instant::now() < deadline, "live peer after {action}");
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(status.signal(), Some(libc::SIGKILL), "{action}");
        assert_eq!(
            probe_process_group_liveness(pid as i32),
            KernelLiveness::Exited
        );
    }
}

#[test]
fn completed_ownership_and_unsafe_group_ids_never_signal_live_peers() {
    let (mut leader, mut peer) = owned_group(false);
    // An invalid signal would return EINVAL if the guard reached killpg. This
    // proves own-group and invalid-id guards without risking the test's group.
    // SAFETY: getpgrp only queries the calling process's group.
    let own = unsafe { libc::getpgrp() };
    for pid in [0, 1, u32::MAX, own as u32, peer.0.id()] {
        OwnedChild::signal_spawned_group(pid, -1).expect("unsafe group id is never signalled");
    }
    // The peer's PID is not a leader: this exercises the same kernel condition
    // as a reused PID now belonging to another group, without forcing reuse.
    // SAFETY: getpgid only queries the peer spawned by this fixture.
    assert_eq!(
        unsafe { libc::getpgid(peer.0.id() as i32) },
        leader.pid() as i32
    );
    assert_ne!(peer.0.id(), leader.pid());
    assert!(peer.0.try_wait().expect("live peer").is_none());

    leader.child_mut().kill().expect("stop leader only");
    leader.child_mut().wait().expect("reap leader");
    leader.mark_reaped();
    leader.signal_group(libc::SIGKILL).expect("disarmed signal");
    leader.kill_and_reap();
    leader.terminate(Duration::ZERO).expect("disarmed TERM");
    drop(leader);
    assert!(
        peer.0
            .try_wait()
            .expect("peer after disarmed cleanup")
            .is_none()
    );
}

#[test]
fn bounded_timeout_reaps_a_child_that_moves_into_the_callers_group() {
    use std::io::{Read, Write};
    use std::net::Shutdown;
    use std::os::unix::net::UnixStream;
    use std::sync::mpsc;
    use std::thread::{self, JoinHandle};

    const TEST: &str = "bounded_timeout_reaps_a_child_that_moves_into_the_callers_group";
    const CHILD_ENV: &str = "ORBIT_MOVED_GROUP_TEST_CHILD";
    if std::env::var_os(CHILD_ENV).is_none() {
        let output_dir = tempfile::tempdir().expect("isolated test output");
        let mut command = Command::new(std::env::current_exe().expect("test executable"));
        command
            .args(["--exact", TEST, "--nocapture"])
            .env(CHILD_ENV, "1");
        let output = orbit_common::test_env::run_child_test(&mut command, TEST, output_dir.path());
        orbit_common::test_env::assert_child_test_passed(
            TEST,
            output.status,
            output.stdout,
            output.stderr,
        );
        return;
    }

    // This re-executed test leads its own group. Surviving the timeout proves
    // teardown never signals the caller's group after the child joins it.
    // SAFETY: getpgrp only queries the calling process's group.
    let own_group = unsafe { libc::getpgrp() };
    assert_eq!(own_group, std::process::id() as i32);
    let (parent, mut child_socket) = UnixStream::pair().expect("readiness socket");
    for socket in [&parent, &child_socket] {
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("bounded readiness wait");
    }

    struct Fixture {
        socket: UnixStream,
        pid: Option<libc::pid_t>,
        worker: Option<JoinHandle<()>>,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = self.socket.shutdown(Shutdown::Both);
            if let Some(pid) = self.pid {
                // SAFETY: only clean up this fixture's still-owned direct
                // child, never its new group. A reaped child gives ECHILD.
                unsafe {
                    if libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) == 0 {
                        libc::kill(pid, libc::SIGKILL);
                    }
                }
            }
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }

    let deadline = Duration::from_millis(100);
    let mut command = Command::new("/bin/sleep");
    command.arg("60");
    // SAFETY: the post-fork callback uses only process syscalls and socket IO
    // on an existing descriptor, without allocation or locks. Command sets
    // the original process group before this callback. Spawn cannot finish
    // (and the bounded runner's deadline cannot start) until we acknowledge.
    unsafe {
        command.pre_exec(move || {
            if libc::setpgid(0, own_group) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            let pid = libc::getpid();
            if libc::getpgid(0) != own_group || own_group == pid {
                return Err(std::io::Error::from(std::io::ErrorKind::InvalidData));
            }
            child_socket.write_all(&pid.to_ne_bytes())?;
            child_socket.read_exact(&mut [0])?;
            Ok(())
        });
    }
    let (tx, rx) = mpsc::sync_channel(1);
    let mut fixture = Fixture {
        socket: parent,
        pid: None,
        worker: Some(thread::spawn(move || {
            let result = run_bounded(&mut command, deadline);
            let _ = tx.send(result);
        })),
    };
    let mut pid_bytes = [0; size_of::<libc::pid_t>()];
    fixture
        .socket
        .read_exact(&mut pid_bytes)
        .expect("child moved");
    let pid = libc::pid_t::from_ne_bytes(pid_bytes);
    fixture.pid = Some(pid);
    // SAFETY: query only the child waiting for this test's acknowledgment.
    assert_eq!(unsafe { libc::getpgid(pid) }, own_group);
    assert_ne!(pid, own_group, "the original child group must be empty");
    let started = Instant::now();
    fixture.socket.write_all(&[1]).expect("start bounded run");
    let result = rx.recv_timeout(deadline + Duration::from_secs(2));
    let elapsed = started.elapsed();
    // On a regression the assertion unwinds through Fixture, which kills the
    // moved child and joins the runner, rather than leaving a hung thread.
    assert!(
        result.is_ok() && elapsed < deadline + Duration::from_secs(2),
        "runner exceeded deadline plus cleanup grace: {elapsed:?}"
    );
    assert!(matches!(
        result.expect("bounded completion"),
        Err(orbit_common::OrbitError::ProcessTimeout {
            timeout_ms: 100,
            ..
        })
    ));
    assert!(elapsed >= deadline, "timeout returned before the deadline");
    // SAFETY: ECHILD proves the runner reaped its child, not just killed it.
    let waited = unsafe { libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) };
    let error = std::io::Error::last_os_error().raw_os_error();
    assert_eq!((waited, error), (-1, Some(libc::ECHILD)));
    fixture.pid = None;
    assert_eq!(probe_process_group_liveness(pid), KernelLiveness::Exited);
}
