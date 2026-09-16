//! `SignalHandlerGuard` must only fan SIGINT/SIGTERM out to process groups
//! the supervisor created. A slot that holds a bare pid — one that never led
//! a group, or a reaped child's pid that the kernel may hand to an unrelated
//! group leader — would let one `killpg` reach processes Orbit never spawned.

use std::io::Read;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use super::super::signal::SignalHandlerGuard;

const HELPER_ENV: &str = "ORBIT_EXEC_SIGNAL_GUARD_HELPER";
const REPORT_ENV: &str = "ORBIT_EXEC_SIGNAL_GUARD_REPORT";

#[test]
fn install_registers_only_a_live_process_group_leader() {
    let mut leader = spawn_sleep(true);
    let mut member = spawn_sleep(false);

    let leader_guard = SignalHandlerGuard::install(leader.id()).expect("install for leader");
    let member_guard = SignalHandlerGuard::install(member.id()).expect("install for member");
    assert!(
        leader_guard.registered(),
        "a child that leads its own group is fanned out to"
    );
    assert!(
        !member_guard.registered(),
        "a child sharing our group is a bare pid, not a pgid"
    );

    // Safety: `getpgrp` only reads this process's group id.
    let own_group = unsafe { libc::getpgrp() } as u32;
    let own_guard = SignalHandlerGuard::install(own_group).expect("install for own group");
    assert!(
        !own_guard.registered(),
        "our own process group is never a fan-out target"
    );

    let reaped = leader.id();
    kill_and_reap(&mut leader);
    let stale_guard = SignalHandlerGuard::install(reaped).expect("install for reaped pid");
    assert!(
        !stale_guard.registered(),
        "a reaped pid is free for reuse and must not be registered"
    );

    kill_and_reap(&mut member);
}

#[test]
fn release_process_group_clears_the_slot_before_drop() {
    let mut leader = spawn_sleep(true);
    let mut guard = SignalHandlerGuard::install(leader.id()).expect("install for leader");
    assert!(guard.registered());

    guard.release_process_group();
    assert!(
        !guard.registered(),
        "the slot is released as soon as the supervisor reaps the child"
    );
    kill_and_reap(&mut leader);
}

/// Runs the delivery check in a child process: raising SIGTERM inside the
/// shared lib-test process would interrupt every other supervised wait.
///
/// The helper builds a group whose leader has exited while a member lives on
/// — the shape a stale slot takes once the reaped pid stops being a leader —
/// plus a live non-leader pid, installs guards for both, and raises SIGTERM.
/// Neither may be signalled, while a real supervised leader still is.
#[test]
fn sigterm_fanout_skips_a_pid_that_does_not_lead_a_live_child_group() {
    if std::env::var_os(HELPER_ENV).is_some() {
        signal_guard_helper();
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let report = dir.path().join("report");
    let exe = std::env::current_exe().expect("current test binary");
    let output = Command::new(&exe)
        .env(HELPER_ENV, "1")
        .env(REPORT_ENV, &report)
        .args([
            "--exact",
            "supervision::tests::signal::sigterm_fanout_skips_a_pid_that_does_not_lead_a_live_child_group",
            "--nocapture",
        ])
        .stdin(Stdio::null())
        // Own group: a fan-out that escaped the helper must not reach this
        // test process.
        .process_group(0)
        .output()
        .expect("spawn signal guard helper");
    assert!(
        output.status.success(),
        "helper failed: {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let report = std::fs::read_to_string(&report).expect("helper report");
    assert!(
        report.contains("leader_signalled=true"),
        "the supervised leader's group must still receive SIGTERM: {report:?}"
    );
    assert!(
        report.contains("survivor_alive=true"),
        "a guard for a pid that is not a live leader must not signal that pid's group: {report:?}"
    );
}

fn signal_guard_helper() {
    // A group whose leader exits immediately while `sleep` lives on in it.
    let mut victim = Command::new("/bin/sh")
        .args(["-c", "sleep 30 >/dev/null 2>&1 & echo $!"])
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn victim group");
    let mut survivor = String::new();
    victim
        .stdout
        .take()
        .expect("victim stdout")
        .read_to_string(&mut survivor)
        .expect("read survivor pid");
    let survivor: u32 = survivor.trim().parse().expect("survivor pid");
    let victim_pid = victim.id();
    victim.wait().expect("reap victim leader");

    let mut leader = spawn_sleep(true);

    // Last drop restores this disposition; SIG_IGN means no re-raise, so the
    // helper exits normally once the report is written. Set only after the
    // children are spawned: an ignored disposition is inherited across exec.
    // Safety: sets the helper's own SIGTERM disposition only.
    unsafe {
        libc::signal(libc::SIGTERM, libc::SIG_IGN);
    }

    // Registration itself is covered above; this helper only observes what
    // the handler delivers.
    let _stale = SignalHandlerGuard::install(victim_pid).expect("install for reaped leader");
    let _non_leader = SignalHandlerGuard::install(survivor).expect("install for non-leader");
    let supervised = SignalHandlerGuard::install(leader.id()).expect("install for leader");

    // Safety: delivers SIGTERM to this helper process, whose disposition is
    // Orbit's handler while the guards above are live.
    let raised = unsafe { libc::raise(libc::SIGTERM) };
    assert_eq!(raised, 0, "raise SIGTERM");
    let handler_ran = supervised.take_signal() == Some(libc::SIGTERM);

    let leader_status = wait_for_exit(&mut leader, Duration::from_secs(2));
    let leader_signalled = leader_status.and_then(|status| status.signal()) == Some(libc::SIGTERM);
    // Give a stray SIGTERM time to take effect before probing the survivor.
    thread::sleep(Duration::from_millis(200));
    // Safety: signal 0 only probes whether the pid exists.
    let survivor_alive = unsafe { libc::kill(survivor as libc::pid_t, 0) } == 0;

    std::fs::write(
        Path::new(&std::env::var_os(REPORT_ENV).expect("report path")),
        format!(
            "handler_ran={handler_ran}\nleader_signalled={leader_signalled}\nsurvivor_alive={survivor_alive}\n"
        ),
    )
    .expect("write report");

    // Safety: tears down the helper's own victim group and leader.
    unsafe {
        libc::killpg(victim_pid as libc::pid_t, libc::SIGKILL);
    }
    kill_and_reap(&mut leader);
}

fn spawn_sleep(own_group: bool) -> Child {
    let mut command = Command::new("/bin/sleep");
    command
        .arg("30")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if own_group {
        command.process_group(0);
    }
    command.spawn().expect("spawn sleep")
}

fn kill_and_reap(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn wait_for_exit(child: &mut Child, deadline: Duration) -> Option<std::process::ExitStatus> {
    let start = Instant::now();
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            return Some(status);
        }
        if start.elapsed() >= deadline {
            return None;
        }
        thread::sleep(Duration::from_millis(10));
    }
}
