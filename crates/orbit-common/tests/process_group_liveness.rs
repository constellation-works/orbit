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

use orbit_common::process::OwnedChild;

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
