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
    EnvironmentMode, ExecRequest, StdinMode, linux_landlock_read_boundary, probe_landlock,
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
        Output::of(self.spawn(profile, script))
    }

    /// Start the confined child without waiting for it, so a test can change
    /// the workspace while the ruleset is already in force.
    fn spawn(&self, profile: &ResolvedFsProfile, script: &str) -> std::process::Child {
        let request = ExecRequest {
            program: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), script.to_string()],
            current_dir: Some(self.root().display().to_string()),
            timeout_ms: Some(10_000),
            stdin_mode: StdinMode::Null,
            environment_mode: EnvironmentMode::ClearAndSet(self.environment.clone()),
            debug: false,
        };
        spawn_under_linux_landlock(&request, &self.root(), profile).expect("spawn confined child")
    }
}

struct Output {
    succeeded: bool,
    stdout: String,
    stderr: String,
}

impl Output {
    fn of(child: std::process::Child) -> Self {
        let output = child.wait_with_output().expect("wait for confined child");
        Self {
            succeeded: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

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
    profile_denying(read, DEFAULT_DENY_READ)
}

fn profile_denying(read: &[&str], denies: &[&str]) -> ResolvedFsProfile {
    let mut rules: Vec<String> = read.iter().map(ToString::to_string).collect();
    rules.extend(denies.iter().map(|rule| format!("!{rule}")));
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

    let boundary = linux_landlock_read_boundary(&fixture.root(), &profile, &fixture.environment)
        .expect("compile boundary");
    assert!(
        !orbit_exec::grants_read(&boundary.grants, &fixture.root().join("visible.txt")),
        "an empty read profile must not grant a workspace file: {:?}",
        boundary.grants
    );
    fixture
        .run(&profile, "cat visible.txt")
        .assert_withheld("PURE_COMPUTE_SENTINEL");
}

/// The fixture the dynamic-name tests share.
///
/// `vault/**` and `src/secret.key` are bounded exclusions: their own segments
/// say which directories they can name into, so the ruleset can carve those
/// directories out before the names exist. `build/` is out of reach, which is
/// what keeps generated files readable.
fn dynamic_fixture() -> (Fixture, ResolvedFsProfile) {
    let fixture = Fixture::new();
    fixture.write("src/main.rs", "ALLOWED_SENTINEL");
    fs::create_dir(fixture.root().join("build")).expect("create build");
    let profile = profile_denying(&["**"], &["vault/**", "src/secret.key"]);
    (fixture, profile)
}

/// Criterion 1, and the gap this task closed. A third party writes a secret
/// into the workspace *after* the child has been admitted, into a directory
/// that did not exist when the ruleset was compiled. An indirect descendant —
/// a grandchild, which is what request-time argv checks never see — must not
/// be able to return it, and the run must still be able to read a file it
/// generates itself. [F2026-09-054]
#[test]
fn a_secret_written_after_admission_is_withheld_from_an_indirect_descendant() {
    if unenforceable() {
        return;
    }
    let (fixture, profile) = dynamic_fixture();

    // The child waits for the writer, then reads the secret through a
    // grandchild and generates a file of its own through another.
    let child = fixture.spawn(
        &profile,
        "n=0; while [ ! -e go ] && [ $n -lt 200 ]; do n=$((n+1)); sleep 0.05; done; \
         sh -c 'cat vault/secret.txt' || echo DESCENDANT_REFUSED; \
         printf GENERATED_SENTINEL > build/out.txt; \
         sh -c 'cat build/out.txt'",
    );

    let secret = fixture.root().join("vault/secret.txt");
    fs::create_dir(fixture.root().join("vault")).expect("concurrent writer creates vault");
    fs::write(&secret, "LATE_SENTINEL").expect("concurrent writer writes secret");
    fs::write(fixture.root().join("go"), "").expect("release the child");

    // The test is only meaningful if the secret really is there to be read.
    assert_eq!(
        fs::read_to_string(&secret).expect("read secret outside the sandbox"),
        "LATE_SENTINEL"
    );

    let output = Output::of(child);
    output.assert_withheld("LATE_SENTINEL");
    output.assert_returned("DESCENDANT_REFUSED");
    output.assert_returned("GENERATED_SENTINEL");
}

/// Criterion 2, create/read/remove. A denied name the child creates itself is
/// no more readable than one someone else creates: the boundary is the name's
/// directory, not who wrote the bytes. Removing it still works, because this
/// ruleset governs reads and leaves writes to the mount namespace.
#[test]
fn a_denied_name_can_be_created_and_removed_but_never_read_back() {
    if unenforceable() {
        return;
    }
    let (fixture, profile) = dynamic_fixture();

    fixture
        .run(
            &profile,
            "mkdir -p vault && printf SELF_WRITTEN_SENTINEL > vault/key; \
             cat vault/key || echo READ_REFUSED; \
             rm -f vault/key && echo REMOVED",
        )
        .assert_withheld("SELF_WRITTEN_SENTINEL");

    fixture
        .run(
            &profile,
            "mkdir -p vault && printf x > vault/key; rm -f vault/key && echo REMOVED",
        )
        .assert_returned("REMOVED");
}

/// Criterion 2, allowed-to-denied rename, stated as the acquisition rule it
/// is. A grant binds to an inode the child could already read, so giving that
/// inode a denied name afterwards does not take the bytes back — the child
/// could have copied them before it renamed anything. What the boundary does
/// guarantee is that no *new* inode arrives at a denied name with a readable
/// ancestor.
#[test]
fn renaming_an_allowed_file_to_a_denied_name_does_not_withdraw_what_was_granted() {
    if unenforceable() {
        return;
    }
    let (fixture, profile) = dynamic_fixture();

    fixture
        .run(
            &profile,
            "mv src/main.rs src/secret.key && cat src/secret.key",
        )
        .assert_returned("ALLOWED_SENTINEL");

    // The other direction is the one that matters, and it is closed: a file
    // that arrives at the denied name without a grant of its own stays unread.
    fixture
        .run(
            &profile,
            "printf FRESH_SENTINEL > src/secret.key; cat src/secret.key || echo REFUSED",
        )
        .assert_withheld("FRESH_SENTINEL");
}

/// Criterion 2, hard links. A hard link is another name for an inode the
/// ruleset already decided on, so it follows the same acquisition rule as a
/// rename — and a link that would *gain* access at its destination is refused
/// by `REFER`.
#[test]
fn a_hard_link_carries_the_access_its_inode_already_had() {
    if unenforceable() {
        return;
    }
    let (fixture, profile) = dynamic_fixture();

    fixture
        .run(
            &profile,
            "ln src/main.rs src/secret.key && cat src/secret.key",
        )
        .assert_returned("ALLOWED_SENTINEL");

    fixture
        .run(
            &profile,
            "mkdir -p vault && printf LINKED_SENTINEL > vault/key; \
             ln vault/key build/laundered.txt 2>/dev/null; \
             cat build/laundered.txt || echo LINK_REFUSED",
        )
        .assert_withheld("LINKED_SENTINEL");
}

/// Criterion 2, aliases. A symlink is resolved where it points, so pointing one
/// from a readable directory at a denied path grants nothing.
#[test]
fn a_symlink_alias_does_not_launder_a_denied_path() {
    if unenforceable() {
        return;
    }
    let (fixture, profile) = dynamic_fixture();

    fixture
        .run(
            &profile,
            "mkdir -p vault && printf ALIASED_SENTINEL > vault/key; \
             ln -s ../vault/key build/alias.txt; \
             cat build/alias.txt || echo ALIAS_REFUSED",
        )
        .assert_withheld("ALIASED_SENTINEL");
}

/// Criterion 2, descriptors and mappings. Both are acquisitions: once the
/// child holds one, no later filesystem rule can revoke it. Stating this is
/// the point — a reader who expects a rename to close an open descriptor would
/// expect the wrong contract.
#[test]
fn an_open_descriptor_and_a_mapping_survive_a_later_rename_to_a_denied_name() {
    if unenforceable() {
        return;
    }
    let (fixture, profile) = dynamic_fixture();

    fixture
        .run(
            &profile,
            "exec 3< src/main.rs; mv src/main.rs src/secret.key; cat <&3",
        )
        .assert_returned("ALLOWED_SENTINEL");

    if !on_path("python3", &fixture.environment) {
        println!("skipping the mapping half: python3 is not on the child PATH");
        return;
    }
    // The descriptor half already consumed `src/main.rs`, so the mapping half
    // brings its own file and its own sentinel.
    fixture.write("src/mapped.rs", "MAPPED_SENTINEL");
    fixture
        .run(
            &profile,
            "python3 -c \"import mmap,os,sys\n\
             f=open('src/mapped.rs','rb')\n\
             m=mmap.mmap(f.fileno(),0,access=mmap.ACCESS_READ)\n\
             os.rename('src/mapped.rs','src/secret.key')\n\
             sys.stdout.write(m[:].decode())\"",
        )
        .assert_returned("MAPPED_SENTINEL");
}

/// Criterion 2, paired positive operations. Carving a directory out for a
/// bounded exclusion must not cost the run the files it already had or the
/// ones it produces where the rule cannot reach.
#[test]
fn the_carve_out_leaves_the_rest_of_the_workspace_usable() {
    if unenforceable() {
        return;
    }
    let (fixture, profile) = dynamic_fixture();
    fixture.write("build/nested/input.txt", "NESTED_SENTINEL");

    fixture
        .run(&profile, "cat src/main.rs")
        .assert_returned("ALLOWED_SENTINEL");
    fixture
        .run(&profile, "cat build/nested/input.txt")
        .assert_returned("NESTED_SENTINEL");
    fixture
        .run(
            &profile,
            "mkdir -p build/deep && printf DEEP_SENTINEL > build/deep/out.txt; \
             sh -c 'cat build/deep/out.txt'",
        )
        .assert_returned("DEEP_SENTINEL");
}

/// Criterion 3. An exclusion whose `**` crosses directories can name a path
/// beneath every directory in the workspace; enforcing it ahead of time would
/// leave nothing readable that the run itself produced. The compiler says so
/// on the boundary rather than letting a caller believe the kernel is holding
/// the whole profile.
#[test]
fn the_boundary_names_the_exclusions_the_kernel_is_not_holding() {
    let fixture = Fixture::new();
    fixture.write("src/main.rs", "code");

    let boundary = linux_landlock_read_boundary(
        &fixture.root(),
        &profile_denying(&["**"], &["vault/**", "**/*.env"]),
        &fixture.environment,
    )
    .expect("compile boundary");

    assert_eq!(boundary.unenforced_exclusions, vec!["**/*.env".to_string()]);
}

/// Criterion 3. A profile the backend cannot compile is a capability failure,
/// not a reason to run the child with no boundary at all.
#[test]
fn a_profile_that_cannot_be_compiled_refuses_to_spawn() {
    if unenforceable() {
        return;
    }
    let fixture = Fixture::new();
    let missing = fixture.root().join("not-a-workspace");

    let request = ExecRequest {
        program: "/bin/echo".to_string(),
        args: vec!["MUST_NOT_RUN".to_string()],
        current_dir: None,
        timeout_ms: Some(1_000),
        stdin_mode: StdinMode::Null,
        environment_mode: EnvironmentMode::ClearAndSet(fixture.environment.clone()),
        debug: false,
    };
    let error = spawn_under_linux_landlock(&request, &missing, &profile(&["**"]))
        .expect_err("a workspace that does not resolve must not spawn");

    assert!(
        error.to_string().contains("landlock workspace"),
        "the error must name what it could not compile: {error}"
    );
}
