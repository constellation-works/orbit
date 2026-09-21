//! Binary coverage for interactive `orbit init` when stdin is a pipe or closed.
//!
//! The hang this guards is an *open* descriptor that never delivers data
//! (F2026-09-114): closed stdin was already handled. These tests drive the
//! real `orbit` binary so the bound sits at process stdin, not the in-memory
//! `BufRead` seam.

#![allow(missing_docs)]
#![cfg(unix)]
// Fixtures use expect/unwrap for concise failure diagnostics.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use orbit_common::test_env;
use tempfile::{TempDir, tempdir};

const HANG_DEADLINE: Duration = Duration::from_secs(8);
const SUCCESS_DEADLINE: Duration = Duration::from_secs(60);
const CLOSED_STDIN_MESSAGE: &str = "stdin closed before an interactive prompt was answered; pass --task-prefix/--machine-name or --non-interactive";

struct IsolatedHome {
    _temp: TempDir,
    home: PathBuf,
    work: PathBuf,
    empty_path: PathBuf,
}

impl IsolatedHome {
    fn new() -> Self {
        let temp = tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let work = temp.path().join("work");
        let empty_path = temp.path().join("empty-path");
        fs::create_dir_all(&home).expect("create home");
        fs::create_dir_all(&work).expect("create work");
        fs::create_dir_all(&empty_path).expect("create empty PATH");
        Self {
            _temp: temp,
            home,
            work,
            empty_path,
        }
    }
}

fn orbit_init(home: &Path, work: &Path, path: &Path) -> Command {
    let mut command = Command::new(assert_cmd::cargo::cargo_bin!("orbit"));
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .current_dir(work)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("PATH", path)
        .env_remove("ORBIT_HOME")
        .arg("init");
    command
}

fn wait_with_deadline(child: &mut Child, deadline: Duration) -> Option<ExitStatus> {
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            return Some(status);
        }
        if start.elapsed() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn read_stdio(child: &mut Child) -> (String, String) {
    let mut stdout = String::new();
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stdout.take() {
        let _ = pipe.read_to_string(&mut stdout);
    }
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    (stdout, stderr)
}

fn combined_output(stdout: &str, stderr: &str) -> String {
    format!("{stdout}{stderr}")
}

fn names_identity_flags(output: &str) -> bool {
    output.contains("--task-prefix")
        && output.contains("--machine-name")
        && output.contains("--non-interactive")
}

/// Fresh `orbit init` with no identity flags and no crew-backed CLI on PATH
/// asks nothing about crews and hits the host-name prompt first; the crew
/// prompts (`StdinPrompter`) share the same guarded read, so a silent open
/// pipe covers both.
#[test]
fn silent_open_stdin_pipe_exits_instead_of_hanging() {
    let fixture = IsolatedHome::new();
    let mut child = orbit_init(&fixture.home, &fixture.work, &fixture.empty_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn orbit init");
    let child_stdin = child.stdin.take().expect("piped stdin stays open");

    let status = wait_with_deadline(&mut child, HANG_DEADLINE)
        .unwrap_or_else(|| panic!("orbit init blocked on an open silent stdin until killed"));
    drop(child_stdin);

    let (stdout, stderr) = read_stdio(&mut child);
    let output = combined_output(&stdout, &stderr);
    assert!(
        !status.success(),
        "silent stdin must fail\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        names_identity_flags(&output),
        "expected flags in the error, got stdout={stdout:?} stderr={stderr:?}"
    );
}

#[test]
fn closed_stdin_keeps_the_existing_message() {
    let fixture = IsolatedHome::new();
    let mut child = orbit_init(&fixture.home, &fixture.work, &fixture.empty_path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn orbit init");

    let status = wait_with_deadline(&mut child, HANG_DEADLINE)
        .unwrap_or_else(|| panic!("orbit init < /dev/null blocked until killed"));
    let (stdout, stderr) = read_stdio(&mut child);
    let output = combined_output(&stdout, &stderr);
    assert!(
        !status.success(),
        "closed stdin must fail\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        output.contains(CLOSED_STDIN_MESSAGE),
        "expected {CLOSED_STDIN_MESSAGE:?}, got stdout={stdout:?} stderr={stderr:?}"
    );
}

#[test]
fn piped_machine_name_and_task_prefix_still_complete_interactive_init() {
    let fixture = IsolatedHome::new();
    let mut child = orbit_init(&fixture.home, &fixture.work, &fixture.empty_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn orbit init");

    {
        let mut stdin = child.stdin.take().expect("piped stdin");
        // PATH has no provider CLIs, so no crew prompt runs [ORB-12719]; the
        // first answers are the identity prompts.
        stdin
            .write_all(b"pipe-host\nZZ\n")
            .expect("write interactive answers");
    }

    let status = wait_with_deadline(&mut child, SUCCESS_DEADLINE)
        .unwrap_or_else(|| panic!("orbit init with a delivering pipe did not finish"));
    let (stdout, stderr) = read_stdio(&mut child);
    assert!(
        status.success(),
        "piped answers must complete init\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );

    let config = fs::read_to_string(fixture.home.join(".orbit").join("config.toml"))
        .expect("config.toml after interactive init");
    assert!(config.contains("[machine]"), "{config}");
    assert!(config.contains("name = \"pipe-host\""), "{config}");
    assert!(config.contains("task_prefix = \"ZZ\""), "{config}");
}
