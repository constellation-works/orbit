#![allow(missing_docs)]
// Fixture setup uses unwrap/expect for readability; `println!` reports a skip
// on a kernel that cannot enforce the ruleset at all.
#![allow(clippy::expect_used, clippy::print_stdout, clippy::unwrap_used)]
#![cfg(target_os = "linux")]

//! Kernel-level behavior of the activity-scoped read boundary. [ORB-11514]
//!
//! These exercise the real Landlock ruleset against real children, because the
//! whole point of the change is that a compiled grant list is not evidence: the
//! child has to actually fail to open the file.

use std::fs;
use std::path::{Path, PathBuf};

use orbit_exec::{
    EnvironmentMode, ExecRequest, StdinMode, linux_landlock_grants, probe_landlock,
    spawn_under_linux_landlock,
};
use orbit_types::policy::ResolvedFsProfile;

const DEFAULT_DENY_READ: &[&str] = &["**/.env", "**/.env.*", "**/*.env", "**/*.env.*"];

struct Fixture {
    workspace: tempfile::TempDir,
    host: tempfile::TempDir,
    environment: Vec<(String, String)>,
}

impl Fixture {
    fn new() -> Self {
        Self {
            workspace: tempfile::tempdir().expect("workspace"),
            host: tempfile::tempdir().expect("host"),
            environment: vec![("PATH".to_string(), "/usr/bin:/bin".to_string())],
        }
    }

    fn with_env(mut self, name: &str, value: &Path) -> Self {
        self.environment
            .push((name.to_string(), value.display().to_string()));
        self
    }

    /// Carry a real host variable into the child environment, which is how an
    /// operator declares a tool state directory.
    fn inheriting(mut self, names: &[&str]) -> Self {
        self.environment.retain(|(name, _)| name != "PATH");
        self.environment.push((
            "PATH".to_string(),
            std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".to_string()),
        ));
        for name in names {
            if let Ok(value) = std::env::var(name) {
                self.environment.push((name.to_string(), value));
            }
        }
        self
    }

    fn root(&self) -> PathBuf {
        self.workspace
            .path()
            .canonicalize()
            .expect("canonical root")
    }

    fn host_root(&self) -> PathBuf {
        self.host.path().canonicalize().expect("canonical host")
    }

    fn write(&self, relative: &str, contents: &str) -> PathBuf {
        let path = self.root().join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(&path, contents).expect("write fixture file");
        path
    }

    /// Run `script` through `sh` inside the confined child and return its
    /// combined output. `sh` is on every shipped activity allowlist, so this is
    /// the shape of the bypass this change closes.
    fn run(&self, profile: &ResolvedFsProfile, script: &str) -> Output {
        let request = ExecRequest {
            program: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), script.to_string()],
            current_dir: Some(self.root().display().to_string()),
            timeout_ms: Some(10_000),
            stdin_mode: StdinMode::Null,
            environment_mode: EnvironmentMode::ClearAndSet(self.environment.clone()),
            debug: false,
        };
        let output = spawn_under_linux_landlock(&request, &self.root(), profile)
            .expect("spawn confined child")
            .wait_with_output()
            .expect("wait for confined child");
        Output {
            succeeded: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }
}

struct Output {
    succeeded: bool,
    stdout: String,
    stderr: String,
}

impl Output {
    fn assert_withheld(&self, sentinel: &str) {
        assert!(
            !self.stdout.contains(sentinel),
            "sentinel `{sentinel}` reached the caller: stdout={:?} stderr={:?}",
            self.stdout,
            self.stderr
        );
    }

    fn assert_returned(&self, sentinel: &str) {
        assert!(
            self.stdout.contains(sentinel),
            "expected `{sentinel}`: succeeded={} stdout={:?} stderr={:?}",
            self.succeeded,
            self.stdout,
            self.stderr
        );
    }
}

fn profile(read: &[&str]) -> ResolvedFsProfile {
    let mut rules: Vec<String> = read.iter().map(ToString::to_string).collect();
    rules.extend(DEFAULT_DENY_READ.iter().map(|rule| format!("!{rule}")));
    ResolvedFsProfile {
        name: "test".to_string(),
        read: rules,
        modify: vec!["**".to_string()],
    }
}

fn on_path(program: &str, environment: &[(String, String)]) -> bool {
    let Some((_, search_path)) = environment.iter().find(|(name, _)| name == "PATH") else {
        return false;
    };
    std::env::split_paths(search_path).any(|dir| dir.join(program).is_file())
}

fn unenforceable() -> bool {
    let probe = probe_landlock();
    if !probe.available {
        println!("skipping: {}", probe.detail);
    }
    !probe.available
}

#[test]
fn a_confined_child_cannot_read_outside_the_workspace() {
    if unenforceable() {
        return;
    }
    let fixture = Fixture::new();
    fixture.write("visible.txt", "WORKSPACE_OK");
    let outside = fixture.host_root().join("secret.txt");
    fs::write(&outside, "HOST_SENTINEL").expect("write host sentinel");
    let profile = profile(&["**"]);

    fixture
        .run(&profile, "cat visible.txt")
        .assert_returned("WORKSPACE_OK");
    fixture
        .run(&profile, &format!("cat {}", outside.display()))
        .assert_withheld("HOST_SENTINEL");
}

/// The reviewed bypass: an allowlisted program is asked to interpret a shell
/// fragment, so no argument ever looks like a path.
#[test]
fn a_git_shell_alias_cannot_reach_a_host_sentinel() {
    if unenforceable() || !Path::new("/usr/bin/git").exists() {
        return;
    }
    let fixture = Fixture::new();
    let outside = fixture.host_root().join("secret.txt");
    fs::write(&outside, "ALIAS_SENTINEL").expect("write host sentinel");
    fs::create_dir(fixture.root().join("repo")).expect("create repo");

    let script = format!(
        "cd repo && git init -q . && git -c alias.probe='!cat {}' probe",
        outside.display()
    );
    fixture
        .run(&profile(&["**"]), &script)
        .assert_withheld("ALIAS_SENTINEL");
}

#[test]
fn a_deny_read_file_is_withheld_and_stays_withheld_after_a_rename() {
    if unenforceable() {
        return;
    }
    let fixture = Fixture::new();
    fixture.write(".env", "DENIED_SENTINEL");
    fixture.write("src/main.rs", "ALLOWED_SENTINEL");
    let profile = profile(&["**"]);

    fixture
        .run(&profile, "cat .env")
        .assert_withheld("DENIED_SENTINEL");
    fixture
        .run(&profile, "cat src/main.rs")
        .assert_returned("ALLOWED_SENTINEL");
    // Renaming within the directory cannot launder the inode: the denied file
    // keeps no readable ancestor for the life of the child.
    fixture
        .run(&profile, "mv .env laundered.txt; cat laundered.txt")
        .assert_withheld("DENIED_SENTINEL");
}

/// `REFER` is handled precisely so a denied file cannot be relocated into a
/// directory that grants more access than the one holding it.
#[test]
fn a_denied_file_cannot_be_moved_into_a_readable_directory() {
    if unenforceable() {
        return;
    }
    let fixture = Fixture::new();
    fixture.write(".env", "RELOCATED_SENTINEL");
    fixture.write("src/main.rs", "code");

    fixture
        .run(
            &profile(&["**"]),
            "mv .env src/laundered.txt; cat src/laundered.txt",
        )
        .assert_withheld("RELOCATED_SENTINEL");
}

/// A generated file has to be usable, or the confinement breaks every build.
#[test]
fn a_file_the_child_generates_is_readable() {
    if unenforceable() {
        return;
    }
    let fixture = Fixture::new();
    fixture.write("src/main.rs", "code");

    fixture
        .run(
            &profile(&["**"]),
            "printf GENERATED_SENTINEL > src/build.out; cat src/build.out",
        )
        .assert_returned("GENERATED_SENTINEL");
}

/// `/proc` is not granted as a tree, because a process can read any same-user
/// process's `environ` — including the Orbit process that launched the child,
/// whose environment may hold provider credentials.
#[test]
fn the_child_cannot_read_another_processs_environment() {
    if unenforceable() {
        return;
    }
    let fixture = Fixture::new();

    fixture
        .run(
            &profile(&["**"]),
            &format!(
                "cat /proc/{}/environ && echo PARENT_ENVIRON_READ",
                std::process::id()
            ),
        )
        .assert_withheld("PARENT_ENVIRON_READ");
}

/// Criterion 3: the programs shipped activity allowlists name still work, and
/// they work for the reasons the host grants claim — `cargo` needs its
/// toolchain home, `gh` and networked `git` need the resolver and trust store.
#[test]
fn allowlisted_programs_still_run_through_the_boundary() {
    if unenforceable() {
        return;
    }
    let fixture = Fixture::new().inheriting(&["HOME", "CARGO_HOME", "RUSTUP_HOME"]);
    fixture.write("a.txt", "needle");
    let profile = profile(&["**"]);

    for (program, script, sentinel) in [
        (
            "git",
            "git init -q . && git status --short && echo GIT_OK",
            "GIT_OK",
        ),
        ("rg", "rg needle a.txt && echo RG_OK", "RG_OK"),
        ("cargo", "cargo --version && echo CARGO_OK", "CARGO_OK"),
        (
            "make",
            "make --version >/dev/null && echo MAKE_OK",
            "MAKE_OK",
        ),
        ("gh", "gh --version >/dev/null && echo GH_OK", "GH_OK"),
        (
            "getent",
            "getent hosts localhost >/dev/null && echo RESOLVER_OK",
            "RESOLVER_OK",
        ),
    ] {
        if !on_path(program, &fixture.environment) {
            continue;
        }
        fixture.run(&profile, script).assert_returned(sentinel);
    }
}

/// Criterion 5: an allowlisted program's declared tool state is readable
/// through the actual spawn boundary, while its neighbours are not.
#[test]
fn declared_tool_state_is_readable_but_its_credentials_and_neighbours_are_not() {
    if unenforceable() {
        return;
    }
    let fixture = Fixture::new();
    let cargo_home = fixture.host_root().join("cargo-home");
    fs::create_dir_all(cargo_home.join("registry")).expect("create registry");
    fs::write(cargo_home.join("config.toml"), "CONFIG_SENTINEL").expect("write config");
    fs::write(cargo_home.join("credentials.toml"), "TOKEN_SENTINEL").expect("write credentials");
    let unrelated = fixture.host_root().join("unrelated.txt");
    fs::write(&unrelated, "UNRELATED_SENTINEL").expect("write unrelated");

    let fixture = fixture.with_env("CARGO_HOME", &cargo_home);
    let profile = profile(&["**"]);

    fixture
        .run(
            &profile,
            &format!("cat {}/config.toml", cargo_home.display()),
        )
        .assert_returned("CONFIG_SENTINEL");
    fixture
        .run(
            &profile,
            &format!("cat {}/credentials.toml", cargo_home.display()),
        )
        .assert_withheld("TOKEN_SENTINEL");
    fixture
        .run(&profile, &format!("cat {}", unrelated.display()))
        .assert_withheld("UNRELATED_SENTINEL");
}

/// A profile that allows nothing must not leak the workspace through a
/// compiled grant, even though the child still needs to execute.
#[test]
fn a_profile_that_reads_nothing_grants_no_workspace_path() {
    if unenforceable() {
        return;
    }
    let fixture = Fixture::new();
    fixture.write("visible.txt", "PURE_COMPUTE_SENTINEL");
    let profile = ResolvedFsProfile {
        name: "pure_compute".to_string(),
        read: Vec::new(),
        modify: Vec::new(),
    };

    let grants =
        linux_landlock_grants(&fixture.root(), &profile, &fixture.environment).expect("grants");
    assert!(
        !orbit_exec::grants_read(&grants, &fixture.root().join("visible.txt")),
        "an empty read profile must not grant a workspace file: {grants:?}"
    );
    fixture
        .run(&profile, "cat visible.txt")
        .assert_withheld("PURE_COMPUTE_SENTINEL");
}
