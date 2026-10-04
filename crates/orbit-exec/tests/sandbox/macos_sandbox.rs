//! What the kernel enforces for a compiled macOS profile, through the real
//! `/usr/bin/sandbox-exec`.
//!
//! A child shell runs under a profile compiled by
//! [`compile_macos_sandbox_profile`] with the agent mask appended by
//! [`append_macos_subpath_mask`], exactly as the spawn path builds it, and
//! reports which reads, listings and writes the kernel let through.
//!
//! A host where `sandbox-exec` cannot apply a profile skips visibly. The macOS
//! CI leg sets `ORBIT_REQUIRE_SANDBOX_EXEC=1`, which turns that skip into a
//! failure (STD-04 §R8).

#![allow(missing_docs)]
#![allow(clippy::expect_used, clippy::print_stdout, clippy::unwrap_used)]
#![cfg(target_os = "macos")]

use std::process::{Child, Stdio};
use std::time::Duration;

use orbit_exec::{
    MacosSandboxSpawnRequest, append_macos_subpath_mask, compile_macos_sandbox_profile,
    sandbox_exec_available, spawn_under_macos_sandbox,
};
use orbit_types::policy::ResolvedFsProfile;
use wait_timeout::ChildExt;

const REQUIRE_ENV: &str = "ORBIT_REQUIRE_SANDBOX_EXEC";
const CHILD_DEADLINE: Duration = Duration::from_secs(30);

#[test]
fn sandbox_exec_denies_masked_trees_and_allows_only_the_granted_write_root() {
    if let Err(reason) = sandbox_exec_can_apply() {
        if std::env::var(REQUIRE_ENV).as_deref() == Ok("1") {
            panic!("{REQUIRE_ENV}=1 but sandbox-exec cannot apply a profile here: {reason}");
        }
        println!("SKIP: sandbox-exec cannot apply a profile on this host: {reason}");
        return;
    }

    // Under the target directory, not `/tmp` or `/private/var/folders`: the
    // compiled profile grants those scratch trees to every agent, so a fixture
    // there could not show a write being refused.
    let fixture = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).expect("fixture root");
    let root = fixture.path().canonicalize().expect("canonical root");
    let masked = root.join("global/state/plugins");
    let secrets = root.join("global/state/plugin-secrets");
    let write_root = root.join("worktree");
    let readable = root.join("source");
    for dir in [&masked, &secrets, &write_root, &readable] {
        std::fs::create_dir_all(dir).expect("fixture dir");
    }
    std::fs::write(masked.join("state.json"), "PLUGIN-STATE").expect("masked file");
    std::fs::write(secrets.join("token"), "PLUGIN-SECRET").expect("secret file");
    std::fs::write(readable.join("README"), "READABLE").expect("readable file");

    let profile = ResolvedFsProfile {
        name: "implementer".to_string(),
        read: vec![format!("{}/**", root.display())],
        modify: vec![format!("{}/**", write_root.display())],
    };
    let mut profile_text = compile_macos_sandbox_profile(&profile, "codex").expect("compile");
    append_macos_subpath_mask(&mut profile_text, &[masked.clone(), secrets.clone()]);

    let script = r#"
probe() { if (eval "$2") >/dev/null 2>&1; then echo "$1:allowed"; else echo "$1:denied"; fi; }
probe read-masked 'cat "$MASKED/state.json"'
probe list-masked 'ls "$MASKED"'
probe write-masked ': > "$MASKED/planted"'
probe read-secret 'cat "$SECRETS/token"'
probe list-secret 'ls "$SECRETS"'
probe write-secret ': > "$SECRETS/planted"'
probe read-source 'cat "$SOURCE/README"'
probe write-source ': > "$SOURCE/planted"'
probe write-root ': > "$WRITE_ROOT/written"'
"#;
    let env = [
        ("PATH".to_string(), "/usr/bin:/bin".to_string()),
        ("MASKED".to_string(), masked.display().to_string()),
        ("SECRETS".to_string(), secrets.display().to_string()),
        ("SOURCE".to_string(), readable.display().to_string()),
        ("WRITE_ROOT".to_string(), write_root.display().to_string()),
    ];
    let (child, _profile_file) = spawn_under_macos_sandbox(MacosSandboxSpawnRequest {
        profile_text: &profile_text,
        program: "/bin/sh",
        args: &["-c".to_string(), script.to_string()],
        env: &env,
        cwd: Some(&write_root),
        stdin: Stdio::null(),
        stdout: Stdio::piped(),
        stderr: Stdio::piped(),
        inherited_fds: &[],
    })
    .expect("spawn under sandbox-exec");
    let (stdout, stderr) = wait_bounded(child);

    let expected = [
        "read-masked:denied",
        "list-masked:denied",
        "write-masked:denied",
        "read-secret:denied",
        "list-secret:denied",
        "write-secret:denied",
        "read-source:allowed",
        "write-source:denied",
        "write-root:allowed",
    ];
    let reported: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        reported, expected,
        "kernel verdicts under the compiled profile\nstderr:\n{stderr}\nprofile:\n{profile_text}"
    );
    // The verdicts are the child's own view; the filesystem confirms them.
    for planted in [
        masked.join("planted"),
        secrets.join("planted"),
        readable.join("planted"),
    ] {
        assert!(
            !planted.exists(),
            "{} must not be created under the sandbox",
            planted.display()
        );
    }
    assert!(
        write_root.join("written").is_file(),
        "the granted write root must accept the child's write"
    );
}

/// `Ok` when `sandbox-exec` runs a permissive profile; otherwise the reason it
/// cannot, for the skip message.
fn sandbox_exec_can_apply() -> Result<(), String> {
    if !sandbox_exec_available() {
        return Err("/usr/bin/sandbox-exec is not available".to_string());
    }
    let (child, _profile_file) = spawn_under_macos_sandbox(MacosSandboxSpawnRequest {
        profile_text: "(version 1)\n(allow default)\n",
        program: "/usr/bin/true",
        args: &[],
        env: &[],
        cwd: None,
        stdin: Stdio::null(),
        stdout: Stdio::null(),
        stderr: Stdio::piped(),
        inherited_fds: &[],
    })
    .map_err(|error| error.to_string())?;
    let mut guard = ChildGuard(child);
    match guard
        .0
        .wait_timeout(CHILD_DEADLINE)
        .map_err(|error| error.to_string())?
    {
        Some(status) if status.success() => Ok(()),
        Some(status) => Err(format!("a permissive profile exited with {status}")),
        None => Err(format!("a permissive profile ran past {CHILD_DEADLINE:?}")),
    }
}

/// Wait for the sandboxed child within [`CHILD_DEADLINE`], then collect its
/// output. The guard kills and reaps it on any early exit.
fn wait_bounded(child: Child) -> (String, String) {
    use std::io::Read;

    let mut guard = ChildGuard(child);
    let status = guard
        .0
        .wait_timeout(CHILD_DEADLINE)
        .expect("wait for sandboxed child")
        .unwrap_or_else(|| panic!("sandboxed child ran past {CHILD_DEADLINE:?}"));
    let mut stdout = String::new();
    let mut stderr = String::new();
    guard
        .0
        .stdout
        .take()
        .expect("stdout pipe")
        .read_to_string(&mut stdout)
        .expect("read stdout");
    guard
        .0
        .stderr
        .take()
        .expect("stderr pipe")
        .read_to_string(&mut stderr)
        .expect("read stderr");
    assert!(
        status.success(),
        "sandboxed shell failed with {status}\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    (stdout, stderr)
}

/// Kills and reaps the sandboxed child however the test leaves.
struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
