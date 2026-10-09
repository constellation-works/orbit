//! What the kernel enforces for a compiled macOS profile, through the real
//! `/usr/bin/sandbox-exec`.
//!
//! A child shell runs under a profile compiled by
//! [`compile_macos_sandbox_profile`] with the agent mask appended by
//! [`append_macos_subpath_mask`], exactly as the spawn path builds it, and
//! reports which reads, listings, writes and renames the kernel let through.
//!
//! A nested sandbox apply refusal (exit 71 with `sandbox_apply`) skips visibly.
//! Other probe failures fail the test. The macOS
//! CI leg sets `ORBIT_REQUIRE_SANDBOX_EXEC=1`, which turns that skip into a
//! failure (STD-04 §R8).

#![allow(missing_docs)]
#![allow(clippy::expect_used, clippy::print_stdout, clippy::unwrap_used)]
#![cfg(target_os = "macos")]

use std::process::{Child, Stdio};
use std::time::Duration;

use orbit_exec::{
    MacosSandboxSpawnRequest, append_macos_subpath_mask, compile_macos_sandbox_profile,
    macos_sandbox_test_guard, spawn_under_macos_sandbox,
};
use orbit_types::policy::ResolvedFsProfile;
use wait_timeout::ChildExt;

const CHILD_DEADLINE: Duration = Duration::from_secs(30);

#[test]
fn sandbox_exec_denies_masked_trees_and_allows_only_the_granted_write_root() {
    if !macos_sandbox_test_guard(
        "sandbox_exec_denies_masked_trees_and_allows_only_the_granted_write_root",
    ) {
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

/// Recovery uses the prepared-root case. Seatbelt must also enforce the
/// original absent-root deny dynamically, rather than silently dropping it.
#[test]
fn sandbox_exec_denies_absent_and_prepared_recovery_orbit_roots() {
    if !macos_sandbox_test_guard("sandbox_exec_denies_absent_and_prepared_recovery_orbit_roots") {
        return;
    }

    for prepared in [false, true] {
        let fixture = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).expect("fixture root");
        let root = fixture.path().canonicalize().expect("canonical root");
        let checkout = root.join("checkout");
        let denied = checkout.join(".orbit");
        std::fs::create_dir(&checkout).expect("checkout");
        if prepared {
            std::fs::create_dir(&denied).expect("prepared deny root");
            std::fs::write(denied.join("existing.txt"), "before").expect("denied file");
            std::fs::create_dir(denied.join("tmp")).expect("artifact scratch root");
        }
        let profile = ResolvedFsProfile {
            name: "recovery".to_string(),
            read: vec![format!("{}/**", checkout.display())],
            modify: vec![
                format!("{}/**", checkout.display()),
                format!("!{}/**", denied.display()),
                format!("{}/**", denied.join("tmp").display()),
                format!("!{}/**/*.env", checkout.display()),
            ],
        };
        let profile_text = compile_macos_sandbox_profile(&profile, "codex").expect("compile");
        let script = r#"
set -eu
deny() { if "$@"; then echo "unexpected write: $*" >&2; exit 91; fi; }
printf reached > "$CHECKOUT/agent-step.txt"
if [ "$PREPARED" = true ]; then
    printf scratch > "$DENIED/tmp/log.txt"
    deny sh -c 'printf secret > "$1/tmp/new.env"' sh "$DENIED"
    deny sh -c 'printf changed > "$1/existing.txt"' sh "$DENIED"
    deny mv "$DENIED" "$CHECKOUT/orbit-moved"
else
    deny mkdir "$DENIED"
fi
deny mkdir -p "$DENIED/nested"
deny sh -c 'printf created > "$1/created.txt"' sh "$DENIED"
"#;
        let env = [
            ("PATH".to_string(), "/usr/bin:/bin".to_string()),
            ("CHECKOUT".to_string(), checkout.display().to_string()),
            ("DENIED".to_string(), denied.display().to_string()),
            ("PREPARED".to_string(), prepared.to_string()),
        ];
        let (child, _profile_file) = spawn_under_macos_sandbox(MacosSandboxSpawnRequest {
            profile_text: &profile_text,
            program: "/bin/sh",
            args: &["-c".to_string(), script.to_string()],
            env: &env,
            cwd: Some(&checkout),
            stdin: Stdio::null(),
            stdout: Stdio::piped(),
            stderr: Stdio::piped(),
            inherited_fds: &[],
        })
        .expect("spawn recovery agent step under sandbox-exec");
        wait_bounded(child);
        assert_eq!(
            std::fs::read_to_string(checkout.join("agent-step.txt")).expect("agent reached"),
            "reached"
        );
        assert!(!denied.join("created.txt").exists());
        assert!(!denied.join("nested").exists());
        assert!(!checkout.join("orbit-moved").exists());
        if prepared {
            assert_eq!(
                std::fs::read_to_string(denied.join("existing.txt")).expect("denied file"),
                "before"
            );
            assert_eq!(
                std::fs::read_to_string(denied.join("tmp/log.txt"))
                    .expect("artifact scratch write"),
                "scratch"
            );
            assert!(!denied.join("tmp/new.env").exists());
        } else {
            assert!(
                !denied.exists(),
                "the agent must not create the absent deny root"
            );
        }
    }
}

/// Seatbelt checks a rename against the moved entry only, and a glob deny is a
/// pathname regex rooted at the workspace. A directory holding an existing
/// match must not move to host scratch, which every profile may write and
/// where the match leaves the regex, to be changed or read there and moved
/// back.
#[test]
fn sandbox_exec_keeps_existing_glob_deny_matches_inside_the_workspace() {
    if !macos_sandbox_test_guard(
        "sandbox_exec_keeps_existing_glob_deny_matches_inside_the_workspace",
    ) {
        return;
    }

    let fixture = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).expect("fixture root");
    let workspace = fixture.path().canonicalize().expect("canonical workspace");
    let away = tempfile::Builder::new()
        .prefix("orbit-glob-escape-")
        .tempdir_in("/private/tmp")
        .expect("host scratch");
    for dir in ["nested/app", "config/keys", "plain"] {
        std::fs::create_dir_all(workspace.join(dir)).expect("fixture dir");
    }
    let env_file = workspace.join("nested/app/.env");
    std::fs::write(&env_file, "TOKEN=1\n").expect(".env");
    std::fs::write(workspace.join("config/keys/api.secret"), "SECRET").expect("secret");

    // `.env` is denied for writes and reads, as in the default policy.
    // `*.secret` is denied for reads only, so its own pins must stop a move.
    let ws = workspace.display();
    let profile = ResolvedFsProfile {
        name: "implementer".to_string(),
        read: vec![format!("{ws}/**"), format!("!{ws}/**/*.secret")],
        modify: vec![format!("{ws}/**"), format!("!{ws}/**/.env")],
    };
    let profile_text = compile_macos_sandbox_profile(&profile, "codex").expect("compile");

    let script = r#"
probe() { if (eval "$2") >/dev/null 2>&1; then echo "$1:allowed"; else echo "$1:denied"; fi; }
probe append-env 'printf x >> "$WS/nested/app/.env"'
probe move-env-parent 'mv "$WS/nested/app" "$AWAY/app" && printf x >> "$AWAY/app/.env"'
probe move-env-grandparent 'mv "$WS/nested" "$AWAY/nested" && printf x >> "$AWAY/nested/app/.env"'
probe read-secret 'cat "$WS/config/keys/api.secret"'
probe move-secret-parent 'mv "$WS/config/keys" "$AWAY/keys" && cat "$AWAY/keys/api.secret"'
probe move-secret-grandparent 'mv "$WS/config" "$AWAY/config" && cat "$AWAY/config/keys/api.secret"'
probe write-beside-env 'printf ok > "$WS/nested/app/notes.txt"'
probe move-unmatched 'mv "$WS/plain" "$WS/plain-moved"'
"#;
    let env = [
        ("PATH".to_string(), "/usr/bin:/bin".to_string()),
        ("WS".to_string(), workspace.display().to_string()),
        ("AWAY".to_string(), away.path().display().to_string()),
    ];
    let (child, _profile_file) = spawn_under_macos_sandbox(MacosSandboxSpawnRequest {
        profile_text: &profile_text,
        program: "/bin/sh",
        args: &["-c".to_string(), script.to_string()],
        env: &env,
        cwd: Some(&workspace),
        stdin: Stdio::null(),
        stdout: Stdio::piped(),
        stderr: Stdio::piped(),
        inherited_fds: &[],
    })
    .expect("spawn under sandbox-exec");
    let (stdout, stderr) = wait_bounded(child);

    let expected = [
        "append-env:denied",
        "move-env-parent:denied",
        "move-env-grandparent:denied",
        "read-secret:denied",
        "move-secret-parent:denied",
        "move-secret-grandparent:denied",
        "write-beside-env:allowed",
        "move-unmatched:allowed",
    ];
    let reported: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        reported, expected,
        "kernel verdicts under the compiled profile\nstderr:\n{stderr}\nprofile:\n{profile_text}"
    );
    assert_eq!(
        std::fs::read_to_string(&env_file).expect(".env after"),
        "TOKEN=1\n",
        "the denied `.env` must be unchanged"
    );
    let moved: Vec<_> = std::fs::read_dir(away.path())
        .expect("list host scratch")
        .map(|entry| entry.expect("scratch entry").file_name())
        .collect();
    assert!(
        moved.is_empty(),
        "nothing may leave the workspace: {moved:?}"
    );
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
