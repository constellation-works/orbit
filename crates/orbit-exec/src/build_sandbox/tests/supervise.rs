use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use crate::build_sandbox::supervise::{BuildLog, run};
use crate::build_sandbox::{BuildPhaseEnd, BuildPhaseNetwork, BuildPhaseRequest, BuildSandboxSpec};

fn request<'a>(
    build_dir: &'a Path,
    timeout: Duration,
    cap: u64,
    env: &'a [(String, String)],
) -> BuildPhaseRequest<'a> {
    BuildPhaseRequest {
        sandbox: BuildSandboxSpec {
            build_dir,
            readable: &[],
            home: None,
            network: BuildPhaseNetwork::None,
        },
        argv: &[],
        env,
        cwd: build_dir,
        timeout,
        build_dir_cap_bytes: cap,
    }
}

fn shell(script: &str) -> Command {
    let mut command = Command::new("/bin/sh");
    command.args(["-c", script]);
    command
}

fn path_env() -> Vec<(String, String)> {
    vec![("PATH".to_string(), "/usr/bin:/bin".to_string())]
}

#[cfg(unix)]
fn process_is_gone(pid: i32) -> bool {
    // SAFETY: signal 0 only checks for existence.
    unsafe { libc::kill(pid, 0) != 0 }
}

/// The log keeps the first and last half of its cap whatever the chunking,
/// so a runaway build cannot grow it and the failure at the end survives.
#[test]
fn the_build_log_keeps_its_head_and_tail_under_any_chunking() {
    let output: Vec<u8> = (0..1000u32)
        .flat_map(|n| format!("{n:04}\n").into_bytes())
        .collect();
    for chunk in [1, 3, 7, 64, 5000] {
        let mut log = BuildLog::with_cap(100);
        for piece in output.chunks(chunk) {
            log.push(piece);
        }
        let rendered = String::from_utf8(log.render()).expect("utf8");
        assert!(
            rendered.starts_with("0000\n0001\n"),
            "chunk {chunk}: {rendered}"
        );
        assert!(
            rendered.ends_with("0998\n0999\n"),
            "chunk {chunk}: {rendered}"
        );
        assert!(
            rendered.contains(&format!(
                "{} bytes of build output elided",
                output.len() - 100
            )),
            "chunk {chunk}: {rendered}"
        );
    }
    let mut small = BuildLog::with_cap(100);
    small.push(b"short");
    assert_eq!(small.render(), b"short");
}

/// A phase that outlives its timeout is killed with everything in its
/// process group, including a background child it started.
#[cfg(unix)]
#[test]
fn a_timed_out_phase_takes_its_process_group_with_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pid_file: PathBuf = dir.path().join("background.pid");
    let env = path_env();
    let mut log = BuildLog::default();
    let started = Instant::now();
    let end = run(
        shell(&format!(
            "sleep 30 & echo $! > {}; echo started; wait",
            pid_file.display()
        )),
        (),
        &request(dir.path(), Duration::from_millis(500), u64::MAX, &env),
        &mut log,
    )
    .expect("run");
    assert_eq!(end, BuildPhaseEnd::TimedOut);
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "the timeout must bound the phase"
    );
    let background: i32 = std::fs::read_to_string(&pid_file)
        .expect("pid")
        .trim()
        .parse()
        .expect("pid number");
    assert!(
        process_is_gone(background),
        "a background child of a timed-out phase must not survive it"
    );
    assert!(String::from_utf8_lossy(&log.render()).contains("started"));
}

/// Writing past the build-directory cap kills the phase.
#[cfg(unix)]
#[test]
fn a_phase_that_outgrows_the_build_directory_cap_is_killed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let env = path_env();
    let mut log = BuildLog::default();
    let end = run(
        shell("head -c 8192 /dev/zero > big; sleep 30"),
        (),
        &request(dir.path(), Duration::from_secs(20), 4096, &env),
        &mut log,
    )
    .expect("run");
    assert_eq!(end, BuildPhaseEnd::BuildDirCapExceeded);
}

/// A phase that crosses the cap and exits before the first polling tick is
/// still refused; the cap cannot be evaded by a short write.
#[cfg(unix)]
#[test]
fn a_fast_phase_cannot_exit_with_an_oversized_build_directory() {
    let dir = tempfile::tempdir().expect("tempdir");
    let env = path_env();
    let mut log = BuildLog::default();
    let end = run(
        shell("head -c 8192 /dev/zero > big"),
        (),
        &request(dir.path(), Duration::from_secs(20), 4096, &env),
        &mut log,
    )
    .expect("run");
    assert_eq!(end, BuildPhaseEnd::BuildDirCapExceeded);
}

/// A build cannot hide over-cap data behind directory permissions, either
/// while running or when it exits between size polls.
#[cfg(unix)]
#[test]
fn unreadable_build_content_fails_closed_during_and_after_a_phase() {
    use std::os::unix::fs::PermissionsExt;

    for tail in ["", "; sleep 30"] {
        let dir = tempfile::tempdir().expect("build dir");
        let hidden = dir.path().join("hidden");
        let env = path_env();
        let mut log = BuildLog::default();
        let end = run(
            shell(&format!(
                "mkdir hidden && head -c 8192 /dev/zero > hidden/big && chmod 000 hidden{tail}"
            )),
            (),
            &request(dir.path(), Duration::from_secs(20), 4096, &env),
            &mut log,
        );
        let read_error = std::fs::read_dir(&hidden).err().map(|error| error.kind());
        // Restore access before any assertions so TempDir can remove the data.
        std::fs::set_permissions(&hidden, std::fs::Permissions::from_mode(0o700))
            .expect("restore hidden directory permissions");
        // Root can read mode-000 directories; on ordinary hosts verify the
        // fixture actually reaches the permission-denied traversal path.
        // SAFETY: geteuid only reads the current process's effective uid.
        if unsafe { libc::geteuid() } != 0 {
            assert_eq!(read_error, Some(std::io::ErrorKind::PermissionDenied));
        }
        assert!(std::fs::metadata(hidden.join("big")).expect("data").len() > 4096);
        assert_eq!(
            end.expect("run"),
            BuildPhaseEnd::BuildDirCapExceeded,
            "unreadable build content must refuse the phase (tail: {tail:?})"
        );
    }
}

/// The size walk only counts entries beneath its root; a symlink cannot make
/// Orbit inspect files outside the build directory.
#[cfg(unix)]
#[test]
fn build_directory_size_does_not_follow_symlinks() {
    use crate::build_sandbox::supervise::tree_exceeds;

    let dir = tempfile::tempdir().expect("build dir");
    let outside = tempfile::tempdir().expect("outside dir");
    std::fs::write(outside.path().join("large"), [0u8; 16]).expect("outside file");
    std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).expect("symlink");

    assert!(
        !tree_exceeds(dir.path(), 0),
        "an outside file reachable only through a symlink is not part of the build directory"
    );
}

/// A path that vanishes while the size walk runs (a live build deleting its
/// temporary files) holds nothing; only unreadable content fails closed.
#[test]
fn a_vanished_path_is_not_counted_as_unmeasurable() {
    use crate::build_sandbox::supervise::tree_exceeds;

    let dir = tempfile::tempdir().expect("build dir");
    assert!(
        !tree_exceeds(&dir.path().join("deleted"), 0),
        "a path that no longer exists must not refuse a phase"
    );
}

/// The phase runs from its requested working directory, which is the source
/// checkout for plugin builds and may differ from the build directory root.
#[cfg(unix)]
#[test]
fn a_phase_uses_its_requested_working_directory() {
    let build_dir = tempfile::tempdir().expect("build dir");
    let cwd = tempfile::tempdir().expect("working directory");
    let cwd = cwd
        .path()
        .canonicalize()
        .expect("physical working directory");
    let env = path_env();
    let mut log = BuildLog::default();
    let request = BuildPhaseRequest {
        sandbox: BuildSandboxSpec {
            build_dir: build_dir.path(),
            readable: &[],
            home: None,
            network: BuildPhaseNetwork::None,
        },
        argv: &[],
        env: &env,
        cwd: &cwd,
        timeout: Duration::from_secs(20),
        build_dir_cap_bytes: u64::MAX,
    };

    let end = run(shell("pwd"), (), &request, &mut log).expect("run");

    assert_eq!(end, BuildPhaseEnd::Exited(0));
    assert_eq!(
        String::from_utf8_lossy(&log.render()).trim(),
        cwd.display().to_string(),
        "a phase must honor its requested cwd"
    );
}

/// The phase sees only the environment it was given, and cannot dump core.
#[cfg(unix)]
#[test]
fn a_phase_gets_only_its_environment_and_no_core_dumps() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _ambient = orbit_common::test_env::scoped([("ORBIT_BUILD_AMBIENT_PROBE", Some("leaked"))]);
    let env = path_env();
    let mut log = BuildLog::default();
    let end = run(
        shell("env; echo core=$(ulimit -c)"),
        (),
        &request(dir.path(), Duration::from_secs(20), u64::MAX, &env),
        &mut log,
    )
    .expect("run");
    assert_eq!(end, BuildPhaseEnd::Exited(0));
    let output = String::from_utf8_lossy(&log.render()).into_owned();
    assert!(!output.contains("ORBIT_BUILD_AMBIENT_PROBE"), "{output}");
    assert!(output.contains("core=0"), "{output}");
}

/// The `fetch` ruleset refuses a TCP connect to any port but 443, which is
/// what keeps loopback and LAN services out of reach on Linux.
#[cfg(target_os = "linux")]
#[test]
fn the_fetch_ruleset_refuses_tcp_to_any_port_but_443() {
    // Nothing to check on a kernel without network Landlock, or without the
    // `/dev/tcp` of bash this test connects through.
    if crate::linux_landlock::abi_version() < crate::NETWORK_LANDLOCK_ABI
        || !Path::new("/bin/bash").exists()
    {
        return;
    }
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
    let port = listener.local_addr().expect("addr").port();
    let connect = |confined: bool| {
        let mut command = Command::new("/bin/bash");
        command.args(["-c", &format!("exec 3<>/dev/tcp/127.0.0.1/{port}")]);
        let _ruleset = confined.then(|| {
            crate::linux_landlock::restrict_child_tcp_connect_to_port(&mut command, 443)
                .expect("ruleset")
        });
        command.status().expect("bash").success()
    };
    assert!(
        connect(false),
        "the unconfined control must reach the listener"
    );
    assert!(
        !connect(true),
        "a fetch phase must not reach a TCP port other than 443"
    );
}
