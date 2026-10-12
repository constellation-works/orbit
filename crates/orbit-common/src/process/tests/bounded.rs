use std::fs;
use std::io;
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
/// assertion panics or a drain fails to return. A pid is signalled only when
/// its argv can be read and contains every required marker, including its
/// unique recorded path. Unreadable argv authorizes nothing. Cleanup signals
/// individual pids, never a discovered group.
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
    if !signal_authorized(&inspect_argv(pid), markers) {
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

/// What a platform argv read can honestly say about one pid.
#[derive(Debug, PartialEq, Eq)]
enum ArgvInspection {
    /// Argv bytes, NUL-separated, with no environment trailer.
    Present(Vec<u8>),
    /// The pid is gone or is an unreaped zombie (`process_is_alive` is false).
    NoProcess,
    /// The process may still be running, but its argv could not be read.
    Unavailable(String),
}

/// How the leak assertion treats one inspection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReapVerdict {
    /// Gone, zombie, or a live pid whose argv no longer contains the marker.
    Reaped,
    /// Alive and still the recorded command.
    StillRecorded,
    /// Inspection failed. This is not evidence that the process exited.
    Uncertain,
}

fn reap_verdict(inspection: &ArgvInspection, marker: &[u8]) -> ReapVerdict {
    match inspection {
        ArgvInspection::NoProcess => ReapVerdict::Reaped,
        ArgvInspection::Unavailable(_) => ReapVerdict::Uncertain,
        ArgvInspection::Present(cmdline) => {
            let still_recorded =
                !marker.is_empty() && cmdline.windows(marker.len()).any(|window| window == marker);
            if still_recorded {
                ReapVerdict::StillRecorded
            } else {
                ReapVerdict::Reaped
            }
        }
    }
}

fn signal_authorized(inspection: &ArgvInspection, markers: &[&[u8]]) -> bool {
    match inspection {
        ArgvInspection::Present(cmdline) => cmdline_matches(cmdline, markers),
        // A missing or unreadable argv never identifies the recorded child.
        ArgvInspection::NoProcess | ArgvInspection::Unavailable(_) => false,
    }
}

fn inspection_from_read(result: io::Result<Vec<u8>>) -> ArgvInspection {
    match result {
        Ok(cmdline) => ArgvInspection::Present(cmdline),
        Err(error)
            if error.kind() == io::ErrorKind::NotFound
                || error.raw_os_error() == Some(libc::ESRCH) =>
        {
            ArgvInspection::NoProcess
        }
        Err(error) => ArgvInspection::Unavailable(error.to_string()),
    }
}

/// `KERN_PROCARGS2` layout: native-endian `argc`, a NUL-terminated executable
/// path, NUL padding, then `argc` NUL-terminated arguments. Bytes after those
/// arguments are the environment and are not part of the command identity.
fn argv_from_procargs2(buf: &[u8]) -> io::Result<Vec<u8>> {
    if buf.len() < 4 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "short procargs2 record",
        ));
    }
    let mut argc_bytes = [0u8; 4];
    argc_bytes.copy_from_slice(&buf[..4]);
    let argc = i32::from_ne_bytes(argc_bytes);
    let Ok(argc) = usize::try_from(argc) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "procargs2 argc out of range",
        ));
    };
    if argc > 4096 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "procargs2 argc out of range",
        ));
    }
    let mut index = 4usize;
    while index < buf.len() && buf[index] != 0 {
        index += 1;
    }
    if index >= buf.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unterminated procargs2 executable path",
        ));
    }
    while index < buf.len() && buf[index] == 0 {
        index += 1;
    }
    let mut argv = Vec::new();
    for argument in 0..argc {
        if index >= buf.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "truncated procargs2 argv",
            ));
        }
        let start = index;
        while index < buf.len() && buf[index] != 0 {
            index += 1;
        }
        if index >= buf.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unterminated procargs2 argument",
            ));
        }
        if argument > 0 {
            argv.push(0);
        }
        argv.extend_from_slice(&buf[start..index]);
        index += 1;
    }
    Ok(argv)
}

#[cfg(unix)]
fn inspect_argv(pid: u32) -> ArgvInspection {
    // Zombies remain visible to `kill(pid, 0)` but can no longer run. Treat
    // them as exited, the same way `process_is_alive` does.
    if !crate::process::identity::process_is_alive(pid) {
        return ArgvInspection::NoProcess;
    }
    let result = read_argv_bytes(pid);
    if let Err(error) = &result {
        tracing::warn!(pid, ?error, "argv read failed");
    }
    inspection_from_read(result)
}

#[cfg(target_os = "linux")]
fn read_argv_bytes(pid: u32) -> io::Result<Vec<u8>> {
    fs::read(format!("/proc/{pid}/cmdline"))
}

#[cfg(target_os = "macos")]
fn read_argv_bytes(pid: u32) -> io::Result<Vec<u8>> {
    // `ps` is setuid and this lane refuses to exec it. `sysctl` KERN_PROCARGS2
    // reads the same arguments without starting a process.
    let pid = libc::pid_t::try_from(pid)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "pid out of range"))?;
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    let mut len: usize = 0;
    let rc = unsafe {
        // Safety: `mib` is the three-integer KERN_PROCARGS2 name. A null old
        // pointer writes only the required byte count into `len`.
        libc::sysctl(
            mib.as_mut_ptr(),
            3 as libc::c_uint,
            std::ptr::null_mut(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    read_argv_with_capacity(pid, len)
}

#[cfg(target_os = "macos")]
fn read_argv_with_capacity(pid: libc::pid_t, mut cap: usize) -> io::Result<Vec<u8>> {
    const MAX_PROCARGS: usize = 1024 * 1024;
    if cap == 0 || cap > MAX_PROCARGS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unexpected procargs2 size",
        ));
    }
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    for _ in 0..4 {
        let mut buf = vec![0u8; cap];
        let mut out_len = cap;
        let rc = unsafe {
            // Safety: `buf` is writable for `out_len` bytes. The kernel writes
            // at most that many and reports the count it stored.
            libc::sysctl(
                mib.as_mut_ptr(),
                3 as libc::c_uint,
                buf.as_mut_ptr().cast(),
                &mut out_len,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc == 0 {
            if out_len == 0 || out_len > buf.len() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "unexpected procargs2 length",
                ));
            }
            buf.truncate(out_len);
            let argv = argv_from_procargs2(&buf);
            // Darwin can return success with an incomplete record when argv
            // grows between the size query and read (including shell startup).
            // A full buffer and invalid record warrant the same bounded growth
            // as ENOMEM; a malformed record never proves that the child exited.
            if argv.is_err() && out_len == cap && cap < MAX_PROCARGS {
                cap = cap.saturating_mul(2).min(MAX_PROCARGS);
                continue;
            }
            return argv;
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ENOMEM) && cap < MAX_PROCARGS {
            cap = cap.saturating_mul(2).min(MAX_PROCARGS);
            continue;
        }
        return Err(error);
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "procargs2 size kept growing",
    ))
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn read_argv_bytes(_pid: u32) -> io::Result<Vec<u8>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "argv inspection is not implemented on this unix",
    ))
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

// Unreadable argv must not count as a reaped child or authorize a signal.
// A truncated KERN_PROCARGS2 record is unreadable, not a different command.
#[test]
fn unreadable_argv_is_not_a_reaped_child_and_authorizes_no_signal() {
    let marker = b"/unique/writer/0".as_slice();
    let markers = [b"cat".as_slice(), marker];
    let recorded = ArgvInspection::Present(b"cat\0/unique/writer/0".to_vec());
    let cases = [
        (
            ArgvInspection::Unavailable("injected unreadable argv".to_string()),
            ReapVerdict::Uncertain,
            false,
            "unreadable argv",
        ),
        (
            ArgvInspection::NoProcess,
            ReapVerdict::Reaped,
            false,
            "missing process",
        ),
        (
            ArgvInspection::Present(b"cat\0/different/writer/0".to_vec()),
            ReapVerdict::Reaped,
            false,
            "pid whose argv lost the recorded marker",
        ),
        (
            recorded,
            ReapVerdict::StillRecorded,
            true,
            "recorded command",
        ),
    ];
    for (inspection, verdict, authorize, label) in cases {
        assert_eq!(
            reap_verdict(&inspection, marker),
            verdict,
            "{label}: inspection must not be treated as a successful exit"
        );
        assert_eq!(
            signal_authorized(&inspection, &markers),
            authorize,
            "{label}: cleanup must signal only a fully identified recorded command"
        );
    }

    let mut buffer = Vec::new();
    buffer.extend_from_slice(&5i32.to_ne_bytes());
    buffer.extend_from_slice(b"/bin/bash\0\0\0\0\0\0\0");
    buffer.extend_from_slice(b"sh\0-c\0read value\0cat\0/tmp/orbit-marker-probe-unique-path\0");
    buffer.extend_from_slice(b"NOT_A_MARKER=1\0");
    let argv = argv_from_procargs2(&buffer).expect("complete procargs2 record");
    assert_eq!(
        argv, b"sh\0-c\0read value\0cat\0/tmp/orbit-marker-probe-unique-path",
        "argv identity must keep every argument and drop the environment"
    );
    assert!(
        !argv
            .windows(b"NOT_A_MARKER".len())
            .any(|window| window == b"NOT_A_MARKER"),
        "environment bytes must not satisfy a marker match"
    );
    let argv_end = buffer.len() - b"NOT_A_MARKER=1\0".len();
    // Drop the last argument's terminator so the record is no longer complete.
    let truncated = &buffer[..argv_end - 1];
    let unreadable = inspection_from_read(argv_from_procargs2(truncated));
    assert_eq!(
        unreadable,
        ArgvInspection::Unavailable("unterminated procargs2 argument".to_string()),
        "a truncated argument record is uncertainty, not a different process"
    );
    assert_eq!(
        reap_verdict(&unreadable, b"/tmp/orbit-marker-probe-unique-path"),
        ReapVerdict::Uncertain
    );
    assert!(!signal_authorized(&unreadable, &markers));
}

// Native fault injection: grow argv between the size query and read without
// relying on a startup race or generating load on the shared host.
#[cfg(target_os = "macos")]
#[test]
fn argv_probe_recovers_when_a_live_child_outgrows_the_queried_buffer() {
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Child, Stdio};

    let _logging =
        tracing::subscriber::set_default(tracing_subscriber::fmt().with_test_writer().finish());

    struct ReapingChild(Child);
    impl Drop for ReapingChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let marker = format!("orbit-argv-growth-{}", "x".repeat(512));
    let mut child = ReapingChild(
        Command::new("sh")
            .args([
                "-c",
                "echo ready; read value; exec sh -c 'echo ready; read value' cat \"$LONG_ARG\"",
                "cat",
                "short",
            ])
            .env("LONG_ARG", &marker)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn controlled argv growth"),
    );
    let mut output = BufReader::new(child.0.stdout.take().expect("readiness pipe"));
    let mut ready = String::new();
    output.read_line(&mut ready).expect("initial readiness");
    assert_eq!(ready.trim(), "ready");
    let pid = libc::pid_t::try_from(child.0.id()).expect("child pid");
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    let mut cap = 0usize;
    // Safety: the null output pointer requests only the required byte count.
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            std::ptr::null_mut(),
            &mut cap,
            std::ptr::null_mut(),
            0,
        )
    };
    assert_eq!(rc, 0, "initial size query: {}", io::Error::last_os_error());
    writeln!(child.0.stdin.as_mut().expect("control pipe")).expect("release exec");
    ready.clear();
    output.read_line(&mut ready).expect("exec readiness");
    assert_eq!(ready.trim(), "ready");

    // Reproduce the old probe's read with the stale capacity. Darwin may
    // report success even though the returned record cannot contain argv.
    let mut buf = vec![0u8; cap];
    let mut len = cap;
    // Safety: buf has len writable bytes; sysctl reports the bytes it stored.
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            buf.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    let stale = if rc == 0 {
        assert!(len <= buf.len());
        argv_from_procargs2(&buf[..len])
    } else {
        Err(io::Error::last_os_error())
    };
    let error = stale.expect_err("a stale buffer must not identify the grown argv");
    tracing::warn!(
        cap,
        returned = len,
        rc,
        ?error,
        "live child stale KERN_PROCARGS2 buffer"
    );
    let unreadable = inspection_from_read(Err(error));
    assert_eq!(
        reap_verdict(&unreadable, marker.as_bytes()),
        ReapVerdict::Uncertain
    );
    assert!(!signal_authorized(
        &unreadable,
        &[b"cat", marker.as_bytes()]
    ));
    let recovered = inspection_from_read(read_argv_with_capacity(pid, cap));
    assert_eq!(
        reap_verdict(&recovered, marker.as_bytes()),
        ReapVerdict::StillRecorded
    );
    assert!(signal_authorized(&recovered, &[b"cat", marker.as_bytes()]));
    assert!(child.0.try_wait().expect("live child status").is_none());
}

// Fault injection: the public runner cannot deterministically fail a native
// argv read between two successful observations, or keep it unreadable.
#[cfg(unix)]
#[test]
fn leak_assertion_retries_unreadable_argv_without_accepting_it_as_reaped() {
    use std::cell::Cell;
    use std::panic::{AssertUnwindSafe, catch_unwind};

    let marker = b"/unique/writer/0";
    let budget = Duration::from_millis(40);
    for (label, recovery, expected) in [
        ("transient read failure", Some(true), Some("still running")),
        (
            "persistently unreadable argv",
            None,
            Some("argv inspection unavailable"),
        ),
        ("exit after an unreadable read", Some(false), None),
    ] {
        let started = Instant::now();
        let elapsed = Cell::new(Duration::ZERO);
        let mut attempts = 0;
        let result = catch_unwind(AssertUnwindSafe(|| {
            assert_reaped_with_probe(
                123,
                marker,
                budget,
                |_| {
                    attempts += 1;
                    if attempts == 1 || recovery.is_none() {
                        inspection_from_read(Err(io::Error::from_raw_os_error(libc::EINTR)))
                    } else if recovery == Some(true) {
                        ArgvInspection::Present(b"cat\0/unique/writer/0".to_vec())
                    } else {
                        ArgvInspection::NoProcess
                    }
                },
                || started + elapsed.get(),
                |delay| elapsed.set(elapsed.get() + delay),
            );
        }));
        assert!(attempts > 1, "{label}: an unreadable read must be retried");
        if let Some(expected) = expected {
            let payload = result.expect_err("unreadable or live argv must not pass the leak check");
            let message = payload.downcast_ref::<String>().expect("panic diagnostic");
            assert!(message.contains(expected), "{label}: {message}");
            assert_eq!(
                elapsed.get(),
                budget,
                "{label}: the whole budget must be used"
            );
            if recovery.is_none() {
                assert!(
                    message.contains(&io::Error::from_raw_os_error(libc::EINTR).to_string()),
                    "the final unreadable probe must retain the underlying error: {message}"
                );
            }
        } else {
            assert!(result.is_ok(), "{label}: a confirmed exit must pass");
        }
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

/// A blocked owned child stays visible without `/proc`. The leak assertion
/// must fail while it is live, and only the watchdog's recorded pid is killed.
#[cfg(unix)]
#[test]
fn blocked_child_leak_check_fails_while_the_writer_lives_and_cleanup_kills_only_that_pid() {
    use std::any::Any;
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::process::{Child, Stdio};

    struct ReapingChild(Child);
    impl Drop for ReapingChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    fn panic_text(payload: &(dyn Any + Send)) -> String {
        payload
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| payload.downcast_ref::<&str>().copied())
            .unwrap_or("non-string panic")
            .to_string()
    }
    let survives = |child: &mut Child, reason: &str| {
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
    // `read` is a shell builtin, so this process blocks with both markers in
    // its own argv and with no descendants.
    let mut writer = ReapingChild(
        Command::new("sh")
            .args(["-c", "read value", "cat"])
            .arg(&marker)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .expect("spawn blocked writer"),
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
    let pid = writer.0.id();
    fs::write(&marker, pid.to_string()).expect("recorded pid");
    fs::write(other_kids.join("0"), pid.to_string()).expect("unmatched pid");
    let marker_bytes = marker.as_os_str().as_encoded_bytes().to_vec();

    // The leak assertion below verifies the recorded marker through the
    // retrying probe. A one-shot argv precheck would reintroduce the flake.
    assert!(
        writer.0.try_wait().expect("writer status").is_none(),
        "writer must still be blocked before the leak check"
    );

    let leaked = catch_unwind(AssertUnwindSafe(|| {
        assert_reaped_within(pid, &marker_bytes, Duration::from_millis(40));
    }));
    let payload = leaked.expect_err("leak assertion must fail while the owned writer is live");
    let text = panic_text(&*payload);
    assert!(
        text.contains("still running"),
        "a live owned child must fail as still running, not as an unreadable probe: {text}"
    );
    assert!(
        writer.0.try_wait().expect("writer status").is_none(),
        "the leak assertion must not kill the writer"
    );
    assert!(
        peer.0.try_wait().expect("peer status").is_none(),
        "the leak assertion must not kill the peer"
    );

    reap_recorded(&leader_path, b"absent-leader", &other_kids);
    survives(
        &mut writer.0,
        "a pid recorded under a path that is not in its argv must not be signalled",
    );
    survives(
        &mut peer.0,
        "unmatched cleanup must not signal other members of the writer's group",
    );

    // The watchdog and `Drop` both call `reap_recorded`. Arm it only after the
    // negative checks so this death is the watchdog's signal.
    let armed = Instant::now();
    let cleanup = FloodCleanup::arm(
        leader_path,
        kids,
        b"absent-leader".to_vec(),
        Duration::from_millis(500),
    );
    let deadline = Instant::now() + Duration::from_secs(2);
    let status = loop {
        if let Some(status) = writer.0.try_wait().expect("writer status") {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "watchdog did not kill the recorded blocked writer"
        );
        thread::sleep(Duration::from_millis(20));
    };
    assert!(
        armed.elapsed() >= Duration::from_millis(400),
        "watchdog signalled before its deadline: {:?}",
        armed.elapsed()
    );
    assert_eq!(
        status.signal(),
        Some(libc::SIGKILL),
        "watchdog must SIGKILL only the recorded writer"
    );
    assert_reaped(pid, &marker_bytes);
    survives(
        &mut peer.0,
        "watchdog must not signal other members of a recorded pid's group",
    );
    drop(cleanup);
    survives(
        &mut peer.0,
        "panic cleanup must not signal other members of a recorded pid's group",
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

/// The recorded process is gone, is a zombie, or its pid was reused by a
/// command that does not contain `marker`.
///
/// Argv that stays unreadable through the grace budget is uncertainty. The
/// assertion panics instead of treating a still-live child on a host without `/proc` as reaped.
#[cfg(unix)]
fn assert_reaped(pid: u32, marker: &[u8]) {
    assert_reaped_within(pid, marker, Duration::from_secs(2));
}

#[cfg(unix)]
fn assert_reaped_within(pid: u32, marker: &[u8], budget: Duration) {
    assert_reaped_with_probe(
        pid,
        marker,
        budget,
        inspect_argv,
        Instant::now,
        thread::sleep,
    );
}

// Inject the probe and clock so a read failure and its recovery can be ordered
// without racing a native sysctl or generating load on the shared host.
#[cfg(unix)]
fn assert_reaped_with_probe(
    pid: u32,
    marker: &[u8],
    budget: Duration,
    mut inspect: impl FnMut(u32) -> ArgvInspection,
    mut now: impl FnMut() -> Instant,
    mut pause: impl FnMut(Duration),
) {
    let deadline = now() + budget;
    loop {
        let inspection = inspect(pid);
        let verdict = reap_verdict(&inspection, marker);
        if verdict == ReapVerdict::Reaped {
            return;
        }
        let remaining = deadline.saturating_duration_since(now());
        if remaining.is_zero() {
            match inspection {
                ArgvInspection::Unavailable(error) => panic!(
                    "pid {pid} argv inspection unavailable ({error}); refusing to treat an unreadable child as reaped"
                ),
                ArgvInspection::Present(cmdline) => panic!(
                    "pid {pid} still running: {}",
                    String::from_utf8_lossy(&cmdline)
                ),
                ArgvInspection::NoProcess => unreachable!("a missing process is reaped"),
            }
        }
        // Neither a live recorded child nor an unreadable probe proves exit.
        // Give a transient read failure the same grace budget as teardown.
        pause(remaining.min(Duration::from_millis(20)));
    }
}

/// Fault injection (test-strategy criterion 2) forces otherwise unreachable
/// pipe and wait errors through the public run boundary. A separately waitable
/// group peer proves no live member survives, without relying on init reaping.
#[test]
fn bounded_supervision_errors_kill_the_group_and_reap_the_leader() {
    use std::cell::RefCell;
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    use std::process::Child;
    use std::rc::Rc;

    use crate::process::OwnedChild;
    use crate::process::bounded::{SupervisionFailure, with_supervision_failure};

    struct Group {
        pid: Option<u32>,
        peer: Child,
    }
    impl Drop for Group {
        fn drop(&mut self) {
            if let Some(pid) = self.pid {
                let _ = OwnedChild::signal_spawned_group(pid, libc::SIGKILL);
                // SAFETY: reap only the leader this fixture spawned, if the
                // supervisor failed to do so. ECHILD is harmless after reap.
                unsafe { libc::waitpid(pid as i32, std::ptr::null_mut(), 0) };
            }
            let _ = self.peer.kill();
            let _ = self.peer.wait();
        }
    }

    for point in [
        SupervisionFailure::PreparePipes,
        SupervisionFailure::PipeRead,
        SupervisionFailure::Wait,
    ] {
        let group = Rc::new(RefCell::new(None));
        let observed = Rc::clone(&group);
        let mut command = Command::new("/bin/sleep");
        command.arg("60");
        let result = with_supervision_failure(
            point,
            move |pid| {
                let peer = Command::new("/bin/sleep")
                    .arg("60")
                    .process_group(pid as i32)
                    .spawn()
                    .expect("spawn waitable group peer before the injected error");
                *observed.borrow_mut() = Some(Group {
                    pid: Some(pid),
                    peer,
                });
            },
            || run_bounded(&mut command, Duration::from_secs(30)),
        );
        assert!(
            matches!(result, Err(OrbitError::Execution(ref message))
                if message == &format!("injected bounded supervision failure: {point:?}")),
            "must reach the intended error: {point:?}, {result:?}"
        );
        let mut group = group.borrow_mut().take().expect("fault reached");
        let pid = group.pid.expect("leader pid");
        // SAFETY: only probes the child created by this call. ECHILD proves
        // cleanup reaped it rather than merely leaving a killed zombie.
        let waited = unsafe { libc::waitpid(pid as i32, std::ptr::null_mut(), libc::WNOHANG) };
        let wait_error = io::Error::last_os_error().raw_os_error();
        let deadline = Instant::now() + Duration::from_secs(3);
        let status = loop {
            if let Some(status) = group.peer.try_wait().expect("wait for peer") {
                break status;
            }
            assert!(Instant::now() < deadline, "peer survived {point:?}");
            thread::sleep(Duration::from_millis(10));
        };
        // SAFETY: signal zero only queries this fixture's group after peer reap.
        let probe = unsafe { libc::killpg(pid as i32, 0) };
        let group_error = io::Error::last_os_error().raw_os_error();
        if probe == -1 && group_error == Some(libc::ESRCH) {
            group.pid = None;
        }
        assert_eq!((waited, wait_error), (-1, Some(libc::ECHILD)), "{point:?}");
        assert_eq!(status.signal(), Some(libc::SIGKILL), "{point:?}");
        assert_eq!((probe, group_error), (-1, Some(libc::ESRCH)), "{point:?}");
        // A disappeared group must be harmless to signal again.
        OwnedChild::signal_spawned_group(pid, libc::SIGKILL).expect("absent group");
    }
}
