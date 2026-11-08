//! Shared helpers for the retained API security and interleaving tests.
use axum::body::to_bytes;
use axum::response::Response;
use serde_json::Value;

const ISOLATED_TEST_ENV: &str = "ORBIT_TEST_WEB_FIXTURE_CHILD";

/// Run the calling test's body in a child of this test binary.
///
/// An explicit-root fixture (`OrbitRuntime::from_roots`, or a global
/// `DashboardState` that opens one lazily) reads ambient authority from the
/// process environment. Inherited from a managed run, that authority can route
/// writes to the live workspace or refuse runtime construction before the
/// behavior under test runs; a temporary root is not process isolation. The
/// child starts with that authority cleared and a disposable `HOME`,
/// `USERPROFILE` and working directory; the parent's environment is untouched.
///
/// Returns `true` inside the child, where the caller runs its body, and
/// `false` in the parent once the child ran exactly that test and passed.
pub(crate) fn enter_isolated_child(module: &str, test: &str) -> bool {
    run_isolated_child(module, test, false).is_none()
}

/// [`enter_isolated_child`], with the child's stdin and stderr on a
/// pseudo-terminal, so the child looks like a person at a terminal
/// (`interactive_terminal()` is true) — the same handles an SSH `-tt` tunnel or
/// a terminal-started `orbit web serve` gives the server [ORB-23047].
#[cfg(unix)]
pub(super) fn enter_isolated_child_on_terminal(module: &str, test: &str) -> bool {
    run_isolated_child(module, test, true).is_none()
}

fn run_isolated_child(module: &str, test: &str, on_terminal: bool) -> Option<String> {
    let module = module
        .strip_prefix(concat!(env!("CARGO_CRATE_NAME"), "::"))
        .unwrap_or(module);
    let exact_test = format!("{module}::{test}");
    if std::env::var_os(ISOLATED_TEST_ENV).is_some_and(|name| name == exact_test.as_str()) {
        return None;
    }

    let home = tempfile::tempdir().expect("isolated fixture home");
    let mut command = std::process::Command::new(std::env::current_exe().expect("test executable"));
    orbit_common::test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command.args(["--exact", &exact_test, "--nocapture", "--test-threads=1"]);
    command
        .env(ISOLATED_TEST_ENV, &exact_test)
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .current_dir(home.path());
    let output = if on_terminal {
        run_on_terminal(command)
    } else {
        command.output().expect("run isolated fixture")
    };
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "isolated `{exact_test}` failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("test result: ok. 1 passed;"),
        "the isolated child must run `{exact_test}` itself, not filter it out:\n{stdout}"
    );
    Some(stdout)
}

/// Run `command` with stdin and stderr on a fresh pseudo-terminal and stdout
/// piped. The master side is drained on a thread so the child never blocks on a
/// full terminal buffer; it ends when the child and this side close the slave.
#[cfg(unix)]
fn run_on_terminal(mut command: std::process::Command) -> std::process::Output {
    use std::io::Read;
    use std::os::fd::FromRawFd;

    let mut master_fd = -1;
    let mut slave_fd = -1;
    // SAFETY: openpty initializes both valid out-pointers on success; the
    // name, termios and window-size arguments may be null.
    let opened = unsafe {
        libc::openpty(
            &mut master_fd,
            &mut slave_fd,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(opened, 0, "open PTY: {}", std::io::Error::last_os_error());
    // SAFETY: openpty succeeded, so `master_fd` is an open descriptor no other
    // handle owns; this File takes sole ownership and closes it.
    let mut master = unsafe { std::fs::File::from_raw_fd(master_fd) };
    // SAFETY: openpty succeeded, so `slave_fd` is the distinct, open slave of
    // that pair that no other handle owns; this File takes sole ownership.
    let slave = unsafe { std::fs::File::from_raw_fd(slave_fd) };
    let stdin_slave = slave.try_clone().expect("clone PTY slave");
    let drain = std::thread::spawn(move || {
        let mut sink = Vec::new();
        // A closed slave reads as EIO on Linux and EOF elsewhere; either ends it.
        let _ = master.read_to_end(&mut sink);
        String::from_utf8_lossy(&sink).into_owned()
    });
    command
        .stdin(stdin_slave)
        .stderr(slave)
        .stdout(std::process::Stdio::piped());
    let child = command.spawn().expect("spawn isolated fixture on a PTY");
    // Close this side's slave handles, held by `command`, so the drain ends.
    drop(command);
    let mut output = child.wait_with_output().expect("run isolated fixture");
    output.stderr = drain.join().expect("drain PTY").into_bytes();
    output
}

#[cfg(not(unix))]
fn run_on_terminal(_command: std::process::Command) -> std::process::Output {
    unreachable!("terminal fixtures are unix-only")
}

/// Refuse an explicit-root fixture outside [`enter_isolated_child`], so a new
/// test cannot silently build one with the launching process's authority.
pub(super) fn assert_isolated_child() {
    assert!(
        std::env::var_os(ISOLATED_TEST_ENV).is_some(),
        "explicit-root web fixtures must run through `enter_isolated_child`"
    );
}

pub(super) async fn body_json(response: Response) -> Value {
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read response body");
    serde_json::from_slice(&bytes).expect("json response")
}

pub(super) fn write_lines(path: &std::path::Path, lines: &[String]) {
    let mut content = String::new();
    for line in lines {
        content.push_str(line);
        content.push('\n');
    }
    std::fs::write(path, content).expect("write fixture");
}
