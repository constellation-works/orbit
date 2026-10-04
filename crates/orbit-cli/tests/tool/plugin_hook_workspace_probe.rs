//! The plugin's `SessionStart` hook decides "is this an Orbit workspace?" with
//! its own filesystem walk. These tests run that script against a checkout that
//! a real `orbit workspace init` produced, so a change to what init writes
//! cannot silently turn the hook into a false alarm on every initialized
//! project.
#![allow(missing_docs)]
// Tests use unwrap/expect to keep fixture setup readable.
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![cfg(unix)]

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use orbit_common::test_env;
use serde_json::Value;
use tempfile::tempdir;

use crate::git_repo;

fn hook_script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("plugin/hooks/check-workspace.sh")
}

fn orbit(work: &Path, home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orbit"));
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .current_dir(work)
        .env("HOME", home)
        .env("USERPROFILE", home);
    command
}

fn run_orbit(command: &mut Command) {
    let output = command.output().expect("spawn orbit");
    assert!(
        output.status.success(),
        "orbit {:?} failed: {}",
        command.get_args().collect::<Vec<_>>(),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Run the hook the way Claude Code does: no `CLAUDE_PROJECT_DIR`, the session
/// directory as the process cwd, an empty stdin.
fn run_hook(cwd: &Path, home: &Path) -> String {
    let output = Command::new("bash")
        .arg(hook_script())
        .current_dir(cwd)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .stdin(Stdio::null())
        .output()
        .expect("spawn hook");
    assert!(
        output.status.success(),
        "the hook must never fail a session start: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("hook stdout is UTF-8")
}

#[test]
fn hook_is_silent_inside_a_workspace_that_orbit_initialized_and_warns_outside_one() {
    let temp = tempdir().expect("tempdir");
    // The hook, like the runtime's initialized-root walk-up, accepts an Orbit
    // root at any ancestor. Under an enclosing `.orbit` (a TMPDIR inside a
    // checkout) it is rightly silent everywhere in the fixture, so neither its
    // warning nor its silence would say anything about the fixture workspace.
    if let Some(enclosing) = temp
        .path()
        .ancestors()
        .map(|dir| dir.join(".orbit"))
        .find(|dir| dir.is_dir())
    {
        writeln!(
            std::io::stderr(),
            "skipped hook workspace probe: {} encloses the fixture, so the hook cannot tell the fixture workspace from its absence",
            enclosing.display()
        )
        .expect("write skip notice");
        return;
    }
    let home = temp.path().join("home");
    let work = temp.path().join("work");
    let elsewhere = temp.path().join("elsewhere");
    for dir in [&home, &elsewhere] {
        std::fs::create_dir_all(dir).expect("create fixture directory");
    }
    git_repo::init(&work);

    run_orbit(orbit(&work, &home).args([
        "init",
        "--non-interactive",
        "--machine-name",
        "hook-probe-host",
        "--task-prefix",
        "TST",
    ]));

    // Before the workspace exists the hook reports it.
    let before = run_hook(&work, &home);
    assert!(
        !before.trim().is_empty(),
        "the hook must speak up in a directory with no Orbit workspace"
    );

    run_orbit(orbit(&work, &home).args(["workspace", "init", "--name", "hook-probe"]));

    let nested = work.join("crates/deep");
    std::fs::create_dir_all(&nested).expect("create nested directory");
    for cwd in [&work, &nested] {
        assert_eq!(
            run_hook(cwd, &home),
            "",
            "an initialized workspace (cwd {}) must produce no hook output",
            cwd.display()
        );
    }

    let outside = run_hook(&elsewhere, &home);
    let report: Value = serde_json::from_str(&outside).expect("hook output is JSON");
    assert_eq!(
        report["continue"], true,
        "the hook must not block the session"
    );
    assert_eq!(
        report["hookSpecificOutput"]["hookEventName"], "SessionStart",
        "agent-visible context is only delivered through the event-specific output"
    );
    assert!(
        report["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .is_some_and(|context| !context.trim().is_empty()),
        "the guidance for the session model must be in additionalContext, not only in the user-facing systemMessage"
    );
    assert!(
        report["systemMessage"]
            .as_str()
            .is_some_and(|message| !message.trim().is_empty()),
        "the user sees the warning through systemMessage"
    );
}
