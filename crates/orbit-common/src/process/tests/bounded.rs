use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use crate::OrbitError;
use crate::process::output_capture::OUTPUT_TRUNCATED_MARKER;
use crate::process::shell::quote_posix_arg;
use crate::process::{run_bounded, run_bounded_capped};

#[cfg(unix)]
#[test]
fn bounded_run_times_out_and_reaps_the_process_group() {
    let dir = tempfile::tempdir().expect("tempdir");
    let leader_path = dir.path().join("leader.pid");
    let child_path = dir.path().join("child.pid");
    let script = dir.path().join("stall.sh");
    let body = format!(
        "#!/bin/sh\necho $$ > {}\nsleep 120 &\necho $! > {}\nwait\n",
        quote_posix_arg(&leader_path.display().to_string()),
        quote_posix_arg(&child_path.display().to_string()),
    );
    fs::write(&script, body).expect("script");

    let deadline = Duration::from_millis(400);
    let started = Instant::now();
    let mut command = Command::new("sh");
    command.arg(&script);
    let error = run_bounded(&mut command, deadline).expect_err("timeout");
    let elapsed = started.elapsed();
    assert!(
        elapsed >= deadline,
        "returned in {elapsed:?}, before the {deadline:?} budget"
    );
    assert!(
        elapsed < deadline + Duration::from_secs(2),
        "returned in {elapsed:?}, past the {deadline:?} budget"
    );
    match error {
        OrbitError::ProcessTimeout { timeout_ms, .. } => {
            assert_eq!(timeout_ms, u64::try_from(deadline.as_millis()).unwrap());
        }
        other => panic!("expected process timeout, got {other}"),
    }

    let leader = read_pid(&leader_path);
    let child = read_pid(&child_path);
    assert_reaped(leader, script.to_str().unwrap().as_bytes());
    assert_reaped(child, b"sleep");
}

#[cfg(unix)]
#[test]
fn bounded_run_returns_when_a_descendant_holds_either_pipe() {
    // Each case leaves a descendant holding exactly one of the output pipes
    // after the leader exits.
    for (label, redirect) in [("stdout", "2>/dev/null"), ("stderr", ">/dev/null")] {
        let dir = tempfile::tempdir().expect("tempdir");
        let child_path = dir.path().join("child.pid");
        let script = dir.path().join("orphan.sh");
        let body = format!(
            "#!/bin/sh\necho ready\nsleep 120 {redirect} &\necho $! > {}\nexit 0\n",
            quote_posix_arg(&child_path.display().to_string()),
        );
        fs::write(&script, body).expect("script");

        let deadline = Duration::from_secs(10);
        let started = Instant::now();
        let mut command = Command::new("sh");
        command.arg(&script);
        let output = run_bounded(&mut command, deadline).expect("leader exit is a finished wait");
        let elapsed = started.elapsed();
        assert!(
            elapsed < deadline,
            "{label}: returned in {elapsed:?}, past the {deadline:?} budget"
        );
        assert!(output.status.success(), "{label}: {:?}", output.status);
        assert_eq!(output.stdout, b"ready\n", "{label}");
        assert_reaped(read_pid(&child_path), b"sleep");
    }
}

#[cfg(unix)]
#[test]
fn bounded_run_drains_output_past_pipe_capacity_with_capped_retention() {
    // 1 MiB per stream is far past any pipe buffer: an undrained child would
    // block on write and hit the deadline instead of exiting.
    let mut command = Command::new("sh");
    command.args([
        "-c",
        "head -c 1048576 /dev/zero; head -c 1048576 /dev/zero >&2; echo done >&2",
    ]);
    let limit = 4096;
    let output =
        run_bounded_capped(&mut command, Duration::from_secs(30), limit).expect("drained run");
    assert!(output.status.success(), "{:?}", output.status);
    for (label, stream) in [("stdout", &output.stdout), ("stderr", &output.stderr)] {
        assert_eq!(
            stream.len(),
            limit + OUTPUT_TRUNCATED_MARKER.len(),
            "{label} retention"
        );
        assert!(stream[..limit].iter().all(|byte| *byte == 0), "{label}");
        assert!(stream.ends_with(OUTPUT_TRUNCATED_MARKER), "{label}");
    }
}

/// Writers that keep a pipe readable must not postpone the deadline. Sixteen
/// `cat /dev/zero` children are what it takes for `read` to stop returning
/// `WouldBlock` on this host; a single writer does not.
#[cfg(unix)]
#[test]
fn bounded_run_enforces_deadline_while_a_stream_stays_readable() {
    for stderr in [false, true] {
        let label = if stderr { "stderr" } else { "stdout" };
        let dir = tempfile::tempdir().expect("tempdir");
        let leader_path = dir.path().join("leader.pid");
        let kids_dir = dir.path().join("kids");
        let ready_path = dir.path().join("ready");
        let script = dir.path().join("flood.sh");
        fs::create_dir(&kids_dir).expect("kids dir");
        write_flood_script(&script, &leader_path, &kids_dir, &ready_path, stderr, false);
        let cleanup = FloodCleanup::arm(
            leader_path.clone(),
            kids_dir.clone(),
            script.as_os_str().as_encoded_bytes().to_vec(),
            Duration::from_secs(12),
        );

        let deadline = Duration::from_millis(800);
        let started = Instant::now();
        let mut command = Command::new("sh");
        command.arg(&script);
        let error = run_bounded_capped(&mut command, deadline, 4096).expect_err("timeout");
        let elapsed = started.elapsed();
        assert!(
            ready_path.exists(),
            "{label}: writers were not running before the deadline"
        );
        assert!(
            elapsed >= deadline,
            "{label}: returned in {elapsed:?}, before the {deadline:?} budget"
        );
        assert!(
            elapsed < deadline + Duration::from_secs(2),
            "{label}: returned in {elapsed:?}, past the {deadline:?} budget while output stayed readable"
        );
        match error {
            OrbitError::ProcessTimeout { timeout_ms, .. } => {
                assert_eq!(timeout_ms, u64::try_from(deadline.as_millis()).unwrap());
            }
            other => panic!("{label}: expected process timeout, got {other}"),
        }

        let leader = read_pid(&leader_path);
        let kids = kid_pids(&kids_dir);
        assert_eq!(kids.len(), FLOOD_WRITERS, "{label}: writer pids");
        assert_reaped(leader, script.as_os_str().as_encoded_bytes());
        for (pid, marker) in kids {
            assert_reaped(pid, marker.as_os_str().as_encoded_bytes());
        }
        drop(cleanup);
    }
}

/// A writer that survives the owned-group signal must not hold the post-exit
/// drain open. `setsid` leaves the process group, so group teardown does not
/// stop it; the drain's own bound has to. macOS ships no `setsid` binary, so
/// the fixture falls back to perl's `POSIX::setsid`.
#[cfg(unix)]
#[test]
fn bounded_run_bounds_post_exit_drain_while_a_detached_writer_continues() {
    for stderr in [false, true] {
        let label = if stderr { "stderr" } else { "stdout" };
        let dir = tempfile::tempdir().expect("tempdir");
        let leader_path = dir.path().join("leader.pid");
        let kids_dir = dir.path().join("kids");
        let ready_path = dir.path().join("ready");
        let script = dir.path().join("detach.sh");
        fs::create_dir(&kids_dir).expect("kids dir");
        write_flood_script(&script, &leader_path, &kids_dir, &ready_path, stderr, true);
        let cleanup = FloodCleanup::arm(
            leader_path.clone(),
            kids_dir.clone(),
            script.as_os_str().as_encoded_bytes().to_vec(),
            Duration::from_secs(12),
        );

        let deadline = Duration::from_secs(10);
        let started = Instant::now();
        let mut command = Command::new("sh");
        command.arg(&script);
        let output = run_bounded_capped(&mut command, deadline, 4096)
            .expect("leader exit is a finished wait");
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_secs(4),
            "{label}: returned in {elapsed:?}; post-exit drain tracked the detached writers"
        );
        assert!(output.status.success(), "{label}: {:?}", output.status);
        assert!(ready_path.exists(), "{label}: writers were not running");
        let flooded = if stderr {
            &output.stderr
        } else {
            &output.stdout
        };
        assert!(
            flooded.ends_with(OUTPUT_TRUNCATED_MARKER),
            "{label}: drain captured no flood output"
        );
        assert_eq!(
            flooded.len(),
            4096 + OUTPUT_TRUNCATED_MARKER.len(),
            "{label}: retention"
        );

        let kids = kid_pids(&kids_dir);
        assert_eq!(kids.len(), FLOOD_WRITERS, "{label}: writer pids");
        assert_reaped(
            read_pid(&leader_path),
            script.as_os_str().as_encoded_bytes(),
        );
        // Closing the pipe delivers SIGPIPE. These writers are outside the
        // owned group, so this is the test's leak check, not group reaping.
        for (pid, marker) in kids {
            assert_reaped(pid, marker.as_os_str().as_encoded_bytes());
        }
        drop(cleanup);
    }
}

#[cfg(unix)]
fn read_pid(path: &std::path::Path) -> u32 {
    fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("pid file {}: {error}", path.display()))
        .trim()
        .parse()
        .expect("pid")
}

#[cfg(unix)]
const FLOOD_WRITERS: usize = 16;

#[cfg(unix)]
fn write_flood_script(
    script: &Path,
    leader: &Path,
    kids: &Path,
    ready: &Path,
    stderr: bool,
    detach: bool,
) {
    let redirect = if stderr { " >&2" } else { "" };
    let mut body = String::from("#!/bin/sh\n");
    if detach {
        body.push_str(
            "detach() {\n  if command -v setsid >/dev/null 2>&1; then\n    setsid \"$@\"\n  else\n    perl -MPOSIX -e 'POSIX::setsid() != -1 or die \"setsid: $!\"; exec @ARGV or die \"exec: $!\"' -- \"$@\"\n  fi\n}\n",
        );
    }
    body.push_str(&format!(
        "echo $$ > {}\n",
        quote_posix_arg(&leader.display().to_string())
    ));
    for index in 0..FLOOD_WRITERS {
        let pid_path = kids.join(index.to_string());
        let quoted_pid = quote_posix_arg(&pid_path.display().to_string());
        if detach {
            let inner = format!("echo $$ > {quoted_pid}; exec cat /dev/zero {quoted_pid}");
            body.push_str(&format!(
                "detach sh -c {}{redirect} &\n",
                quote_posix_arg(&inner)
            ));
        } else {
            body.push_str(&format!(
                "cat /dev/zero {quoted_pid}{redirect} &\necho $! > {quoted_pid}\n"
            ));
        }
    }
    if detach {
        // The leader exits only after the detached writers exist, so the
        // post-exit drain runs against a pipe that is still being filled.
        body.push_str("tries=0\nwhile [ \"$tries\" -lt 40 ]; do\n  missing=0\n  i=0\n");
        body.push_str(&format!(
            "  while [ \"$i\" -lt {FLOOD_WRITERS} ]; do\n    if [ ! -s {}/$i ]; then missing=1; break; fi\n    i=$((i + 1))\n  done\n",
            quote_posix_arg(&kids.display().to_string())
        ));
        body.push_str(
            "  if [ \"$missing\" -eq 0 ]; then break; fi\n  sleep 0.05\n  tries=$((tries + 1))\ndone\nsleep 0.2\n",
        );
    }
    body.push_str(&format!(
        "echo ready > {}\n",
        quote_posix_arg(&ready.display().to_string())
    ));
    if detach {
        body.push_str("exit 0\n");
    } else {
        body.push_str("wait\n");
    }
    fs::write(script, body).expect("script");
}

/// Kills recorded children if the supervisor does not, including when an
/// assertion panics or a drain fails to return. A pid is signalled only while
/// its command line contains every required marker, including its unique
/// recorded path. Cleanup signals individual pids, never a discovered group.
#[cfg(unix)]
struct FloodCleanup {
    cancel: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
    leader_path: PathBuf,
    kids_dir: PathBuf,
    leader_marker: Vec<u8>,
}

#[cfg(unix)]
impl FloodCleanup {
    fn arm(
        leader_path: PathBuf,
        kids_dir: PathBuf,
        leader_marker: Vec<u8>,
        hard: Duration,
    ) -> Self {
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&cancel);
        let leader = leader_path.clone();
        let kids = kids_dir.clone();
        let marker = leader_marker.clone();
        let handle = thread::spawn(move || {
            let deadline = Instant::now() + hard;
            while Instant::now() < deadline {
                if flag.load(Ordering::SeqCst) {
                    return;
                }
                thread::sleep(Duration::from_millis(20));
            }
            if flag.load(Ordering::SeqCst) {
                return;
            }
            reap_recorded(&leader, &marker, &kids);
        });
        Self {
            cancel,
            handle: Some(handle),
            leader_path,
            kids_dir,
            leader_marker,
        }
    }
}

#[cfg(unix)]
impl Drop for FloodCleanup {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        reap_recorded(&self.leader_path, &self.leader_marker, &self.kids_dir);
    }
}

#[cfg(unix)]
fn reap_recorded(leader_path: &Path, leader_marker: &[u8], kids_dir: &Path) {
    if let Some(pid) = read_pid_optional(leader_path) {
        kill_if_cmdline(pid, &[leader_marker]);
    }
    let Ok(entries) = fs::read_dir(kids_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let marker = entry.path();
        let Some(pid) = read_pid_optional(&marker) else {
            continue;
        };
        kill_if_cmdline(pid, &[b"cat", marker.as_os_str().as_encoded_bytes()]);
    }
}

#[cfg(unix)]
fn kill_if_cmdline(pid: u32, markers: &[&[u8]]) {
    if !cmdline_has(pid, markers) {
        return;
    }
    let Some(pid) = i32::try_from(pid).ok().filter(|pid| *pid > 1) else {
        return;
    };
    // The recorded pid can belong to a group we did not create (including
    // the test runner's group). Matching its argv grants no ownership of the
    // other group members, so only signal this individual process.
    // Safety: `kill` signals a positive process id without dereferencing memory.
    unsafe {
        libc::kill(pid, libc::SIGKILL);
    }
}

#[cfg(unix)]
fn cmdline_has(pid: u32, markers: &[&[u8]]) -> bool {
    let Ok(cmdline) = fs::read(format!("/proc/{pid}/cmdline")) else {
        return false;
    };
    cmdline_matches(&cmdline, markers)
}

fn cmdline_matches(cmdline: &[u8], markers: &[&[u8]]) -> bool {
    !markers.is_empty()
        && markers.iter().all(|marker| {
            !marker.is_empty()
                && cmdline
                    .windows(marker.len())
                    .any(|window| window == *marker)
        })
}

// Safety invariant of the test-only cleanup, unreachable through run_bounded:
// generic command names must never substitute for the unique writer marker.
#[test]
fn flood_cleanup_requires_every_nonempty_marker() {
    let cmdline = b"/usr/bin/cat\0/dev/zero\0/unique/writer/0\0";
    for (markers, expected) in [
        (vec![b"cat".as_slice(), b"/unique/writer/0"], true),
        (vec![b"cat".as_slice(), b"/different/writer/0"], false),
        (vec![b"sleep".as_slice(), b"/unique/writer/0"], false),
        (vec![b"cat".as_slice(), b""], false),
        (vec![], false),
    ] {
        assert_eq!(
            cmdline_matches(cmdline, &markers),
            expected,
            "cleanup must require every nonempty marker: {markers:?}"
        );
    }
}

// Native kernel coverage of the cleanup helper itself: a recorded pid may
// share a group with a process that the flood fixture does not own.
#[cfg(target_os = "linux")]
#[test]
fn flood_cleanup_preserves_unmarked_pids_and_other_group_members() {
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    use std::process::{Child, Stdio};

    struct ReapingChild(Child);
    impl Drop for ReapingChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let assert_survives = |child: &mut Child, reason: &str| {
        // A group signal can be observed by its members at different times.
        // Observe survival rather than accepting one immediate try_wait poll.
        let deadline = Instant::now() + Duration::from_millis(200);
        loop {
            assert!(
                child.try_wait().expect("child status").is_none(),
                "{reason}"
            );
            if Instant::now() >= deadline {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
    };

    let dir = tempfile::tempdir().expect("tempdir");
    let kids = dir.path().join("kids");
    let other_kids = dir.path().join("other-kids");
    fs::create_dir(&kids).expect("kids dir");
    fs::create_dir(&other_kids).expect("other kids dir");
    let marker = kids.join("0");
    let leader_path = dir.path().join("absent-leader.pid");
    // read is a shell builtin, so this process remains blocked without any
    // descendants, with both required markers in its actual argv.
    let mut writer = ReapingChild(
        Command::new("sh")
            .args(["-c", "read value", "cat"])
            .arg(&marker)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .expect("spawn recorded writer"),
    );
    let group = i32::try_from(writer.0.id()).expect("process group id");
    let mut peer = ReapingChild(
        Command::new("sleep")
            .arg("120")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(group)
            .spawn()
            .expect("spawn peer in writer group"),
    );
    fs::write(other_kids.join("0"), writer.0.id().to_string()).expect("unmatched pid");
    reap_recorded(&leader_path, b"absent-leader", &other_kids);
    assert_survives(
        &mut writer.0,
        "generic cat marker must not authorize signaling an unmarked pid",
    );

    fs::write(&marker, writer.0.id().to_string()).expect("recorded pid");
    reap_recorded(&leader_path, b"absent-leader", &kids);
    let deadline = Instant::now() + Duration::from_secs(2);
    let status = loop {
        if let Some(status) = writer.0.try_wait().expect("writer status") {
            break status;
        }
        assert!(Instant::now() < deadline, "marked writer was not killed");
        thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(status.signal(), Some(libc::SIGKILL));
    assert_survives(
        &mut peer.0,
        "cleanup must not signal other members of a recorded pid's group",
    );
}

#[cfg(unix)]
fn kid_pids(dir: &Path) -> Vec<(u32, PathBuf)> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut pids = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if let Some(pid) = read_pid_optional(&path) {
            pids.push((pid, path));
        }
    }
    pids.sort_unstable_by_key(|(pid, _)| *pid);
    pids
}

#[cfg(unix)]
fn read_pid_optional(path: &Path) -> Option<u32> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// The recorded process is gone, or its pid was reused by something else.
#[cfg(unix)]
fn assert_reaped(pid: u32, marker: &[u8]) {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match fs::read(format!("/proc/{pid}/cmdline")) {
            Err(_) => return,
            Ok(cmdline) if !cmdline.windows(marker.len()).any(|window| window == marker) => {
                return;
            }
            Ok(cmdline) if Instant::now() >= deadline => {
                panic!(
                    "pid {pid} still running: {}",
                    String::from_utf8_lossy(&cmdline)
                );
            }
            Ok(_) => thread::sleep(Duration::from_millis(20)),
        }
    }
}
