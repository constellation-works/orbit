//! Script steps phase 3 at the exported script-sandbox boundary
//! (`docs/design/script-steps/1_scope.md` §3, §7, §9 item 5):
//!
//! - a script runs under `RLIMIT_CPU` and `RLIMIT_NOFILE` on Linux and macOS,
//!   and cannot open a descriptor past the open-file limit or raise it;
//! - a granted root is probed: a missing one, one inside a credential path and
//!   a write root this process cannot write are refused, naming the path;
//! - on macOS, `macos-sandbox-script-v1` confines the script to its cwd tree,
//!   scratch directory and granted roots, with `.orbit/`, the credential paths
//!   and the network denied; a network grant is refused before anything runs;
//!   and a start nested in another Seatbelt sandbox is refused and runs
//!   nothing.
//!
//! A host where the kernel sandbox cannot be applied (an agent lane's own
//! sandbox) skips the kernel checks with the shared deferral notice. The
//! macOS CI leg sets `ORBIT_REQUIRE_SANDBOX_EXEC=1`, which turns that skip
//! into a failure.

#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::path::{Path, PathBuf};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::time::Duration;

#[cfg(any(target_os = "linux", target_os = "macos"))]
use orbit_exec::{
    ScriptRunEnd, ScriptSandboxNetwork, ScriptSandboxRequest, ScriptSandboxSpec,
    run_script_sandboxed,
};

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn script_sandbox_available(test: &str) -> bool {
    #[cfg(target_os = "linux")]
    {
        let probe = orbit_exec::probe_bwrap();
        if !probe.available {
            orbit_exec::report_bwrap_deferral(test, &probe.detail);
            return false;
        }
    }
    #[cfg(target_os = "macos")]
    if !orbit_exec::macos_sandbox_test_guard(test) {
        return false;
    }
    true
}

/// A physical scratch directory holding every scratch subdirectory.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn scratch_in(parent: &Path) -> PathBuf {
    let scratch = parent.join("scratch");
    for name in orbit_exec::SCRIPT_SCRATCH_DIRS {
        std::fs::create_dir_all(scratch.join(name)).expect("scratch directory");
    }
    scratch.canonicalize().expect("physical scratch")
}

/// The script process runs under the design's `RLIMIT_*` limits: the
/// open-file limit at [`orbit_exec::SCRIPT_OPEN_FILES_LIMIT`] (a descriptor
/// past it cannot be opened, and the limit cannot be raised) and the CPU
/// limit at the step's timeout in seconds.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn a_script_runs_under_its_cpu_and_open_file_limits() {
    if !script_sandbox_available("a_script_runs_under_its_cpu_and_open_file_limits") {
        return;
    }
    let dir = tempfile::tempdir().expect("fixture");
    let cwd = dir.path().join("work");
    std::fs::create_dir_all(&cwd).expect("cwd");
    let cwd = cwd.canonicalize().expect("physical cwd");
    let scratch = scratch_in(dir.path());
    let limit = orbit_exec::SCRIPT_OPEN_FILES_LIMIT;
    let script = format!(
        "echo files=$(ulimit -n) cpu=$(ulimit -t); \
         (exec {below}</dev/null) 2>/dev/null && echo below=opened || echo below=refused; \
         (exec {above}</dev/null) 2>/dev/null && echo above=opened || echo above=refused; \
         (ulimit -n {raise}) 2>/dev/null && echo raise=allowed || echo raise=refused",
        below = limit - 24,
        above = limit + 24,
        raise = limit * 2,
    );
    let argv = vec!["/bin/bash".to_string(), "-c".to_string(), script];
    let env = vec![("PATH".to_string(), "/usr/bin:/bin".to_string())];
    let run = run_script_sandboxed(ScriptSandboxRequest {
        sandbox: ScriptSandboxSpec {
            cwd: &cwd,
            cwd_writable: true,
            scratch: &scratch,
            readable: &[],
            writable: &[],
            masked: &[],
            home: None,
            inputs: &[],
            network: ScriptSandboxNetwork::None,
        },
        argv: &argv,
        env: &env,
        launcher: &[],
        launcher_env: &[],
        stdin: Vec::new(),
        timeout: Duration::from_secs(37),
        scratch_cap_bytes: u64::MAX,
    })
    .expect("run the script");
    let stdout = String::from_utf8_lossy(&run.stdout).into_owned();
    assert_eq!(
        run.end,
        ScriptRunEnd::Exited(0),
        "{}",
        String::from_utf8_lossy(&run.log)
    );
    assert_eq!(
        stdout.lines().collect::<Vec<_>>(),
        [
            format!("files={limit} cpu=37").as_str(),
            "below=opened",
            "above=refused",
            "raise=refused",
        ],
        "the script must run under RLIMIT_NOFILE={limit} and RLIMIT_CPU=37s"
    );
}

/// `orbit script grant` and the job's process probe a granted root the
/// same way: a missing root, one inside a credential path and a write root
/// this process cannot write are refused, each naming the path; a usable
/// root resolves to its physical path.
#[cfg(unix)]
#[test]
fn granted_roots_are_probed_for_existence_credentials_and_access() {
    use orbit_exec::{ScriptRootAccess, probe_script_root};
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().expect("fixture");
    let home = dir.path().join("home");
    let library = dir.path().join("library");
    let sealed = dir.path().join("sealed");
    for path in [&home.join(".ssh/keys"), &library, &sealed] {
        std::fs::create_dir_all(path).expect("fixture directory");
    }
    let home_os = Some(home.as_os_str());

    let physical = probe_script_root(&library, ScriptRootAccess::Write, home_os)
        .expect("a readable, writable directory passes");
    assert_eq!(physical, library.canonicalize().expect("physical"));
    assert_eq!(
        std::fs::read_dir(&library).expect("list").count(),
        0,
        "the write probe leaves nothing behind"
    );

    let missing = dir.path().join("gone");
    let refused = probe_script_root(&missing, ScriptRootAccess::Read, home_os)
        .expect_err("a missing root is refused");
    assert!(
        refused.contains(&missing.display().to_string()) && refused.contains("does not exist"),
        "{refused}"
    );

    let credential = home.join(".ssh/keys");
    let refused = probe_script_root(&credential, ScriptRootAccess::Read, home_os)
        .expect_err("a root inside a credential path is refused");
    assert!(
        refused.contains(&credential.display().to_string()) && refused.contains("credential"),
        "{refused}"
    );

    let refused = probe_script_root(Path::new("relative"), ScriptRootAccess::Read, home_os)
        .expect_err("a relative root is refused");
    assert!(refused.contains("not an absolute path"), "{refused}");

    // SAFETY: `geteuid` has no preconditions.
    if unsafe { libc::geteuid() } == 0 {
        // Root ignores directory modes; the write-probe refusal needs a
        // process that permissions bind.
        return;
    }
    std::fs::set_permissions(&sealed, std::fs::Permissions::from_mode(0o555)).expect("chmod");
    probe_script_root(&sealed, ScriptRootAccess::Read, home_os)
        .expect("a read-only directory is a usable read root");
    let refused = probe_script_root(&sealed, ScriptRootAccess::Write, home_os)
        .expect_err("a write root this process cannot write is refused");
    std::fs::set_permissions(&sealed, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    assert!(
        refused.contains(&sealed.display().to_string()) && refused.contains("not accessible"),
        "{refused}"
    );
    if cfg!(target_os = "macos") {
        assert!(
            refused.contains("Privacy & Security > Full Disk Access"),
            "a macOS denial names the privacy pane to grant it in: {refused}"
        );
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use std::path::Path;
    use std::time::Duration;

    use orbit_common::OrbitError;
    use orbit_exec::{
        ScriptRunEnd, ScriptSandboxInput, ScriptSandboxNetwork, ScriptSandboxRequest,
        ScriptSandboxSpec, compile_macos_script_profile, probe_script_sandbox,
        probe_script_sandbox_network, run_script_sandboxed,
    };
    use orbit_types::workflow::activity_job::SCRIPT_PROFILE_MACOS;

    use super::{scratch_in, script_sandbox_available};

    fn spec<'a>(
        cwd: &'a Path,
        scratch: &'a Path,
        writable: &'a [std::path::PathBuf],
        masked: &'a [std::path::PathBuf],
        home: &'a Path,
        inputs: &'a [ScriptSandboxInput],
        network: ScriptSandboxNetwork,
    ) -> ScriptSandboxSpec<'a> {
        ScriptSandboxSpec {
            cwd,
            cwd_writable: true,
            scratch,
            readable: &[],
            writable,
            masked,
            home: Some(home.as_os_str()),
            inputs,
            network,
        }
    }

    fn request<'a>(
        sandbox: ScriptSandboxSpec<'a>,
        argv: &'a [String],
        env: &'a [(String, String)],
    ) -> ScriptSandboxRequest<'a> {
        ScriptSandboxRequest {
            sandbox,
            argv,
            env,
            launcher: &[],
            launcher_env: &[],
            stdin: Vec::new(),
            timeout: Duration::from_secs(30),
            scratch_cap_bytes: u64::MAX,
        }
    }

    /// A granted script writes its cwd worktree, its scratch `out/` and its
    /// granted write root, reads its copied inputs, and nothing else: no
    /// write beside them or into `in/`, `snapshot/` or `.git`, no read of
    /// the cwd's `.orbit/`, the global root, a credential path or a sibling
    /// directory, and no TCP connection, even to loopback.
    #[test]
    fn a_script_reaches_only_its_granted_paths_and_no_network() {
        if !script_sandbox_available("a_script_reaches_only_its_granted_paths_and_no_network") {
            return;
        }
        let dir = tempfile::tempdir().expect("fixture");
        let root = dir.path().canonicalize().expect("physical fixture");
        let cwd = root.join("work");
        let home = root.join("home");
        let global = home.join(".orbit");
        let outbox = root.join("outbox");
        let beside = root.join("beside");
        let brief_dir = root.join("briefs");
        for path in [
            &cwd.join(".orbit"),
            &cwd.join(".git"),
            &global,
            &home.join(".ssh"),
            &outbox,
            &beside,
            &brief_dir.join("nested"),
        ] {
            std::fs::create_dir_all(path).expect("fixture directory");
        }
        std::fs::write(cwd.join(".orbit/secret"), "state-secret").expect("state");
        std::fs::write(global.join("secret"), "global-secret").expect("global");
        std::fs::write(home.join(".ssh/id_ed25519"), "ssh-secret").expect("key");
        std::fs::write(beside.join("secret"), "beside-secret").expect("beside");
        std::fs::write(brief_dir.join("nested/brief.txt"), "brief").expect("brief");
        let scratch = scratch_in(&root);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
        let port = listener.local_addr().expect("listener addr").port();

        let s = scratch.display();
        let script = format!(
            "probe() {{ if eval \"$2\" >/dev/null 2>&1; then echo \"$1=yes\"; else echo \"$1=no\"; fi; }}; \
             probe cwd_write 'echo x > {cwd}/made'; \
             probe out_write 'echo x > {s}/out/made'; \
             probe outbox_write 'echo x > {outbox}/made'; \
             probe input_read 'grep -q brief {s}/in/briefs/nested/brief.txt'; \
             probe input_write 'echo x > {s}/in/briefs/new'; \
             probe snapshot_write 'echo x > {s}/snapshot/new'; \
             probe git_write 'echo x > {cwd}/.git/new'; \
             probe beside_write 'echo x > {beside}/new'; \
             probe state_read 'cat {cwd}/.orbit/secret'; \
             probe global_read 'cat {global}/secret'; \
             probe credential_read 'cat {home}/.ssh/id_ed25519'; \
             probe beside_read 'cat {beside}/secret'; \
             probe network '(exec 3<>/dev/tcp/127.0.0.1/{port})'",
            cwd = cwd.display(),
            outbox = outbox.display(),
            beside = beside.display(),
            global = global.display(),
            home = home.display(),
        );
        let argv = vec!["/bin/bash".to_string(), "-c".to_string(), script];
        let env = vec![("PATH".to_string(), "/usr/bin:/bin".to_string())];
        let writable = vec![outbox.clone()];
        let masked = vec![cwd.join(".orbit"), global.clone()];
        let inputs = vec![ScriptSandboxInput {
            source: brief_dir.clone(),
            name: "briefs".to_string(),
        }];
        let run = run_script_sandboxed(request(
            spec(
                &cwd,
                &scratch,
                &writable,
                &masked,
                &home,
                &inputs,
                ScriptSandboxNetwork::None,
            ),
            &argv,
            &env,
        ))
        .expect("run the script");

        let stdout = String::from_utf8_lossy(&run.stdout).into_owned();
        assert_eq!(
            run.end,
            ScriptRunEnd::Exited(0),
            "{}",
            String::from_utf8_lossy(&run.log)
        );
        assert_eq!(
            stdout.lines().collect::<Vec<_>>(),
            [
                "cwd_write=yes",
                "out_write=yes",
                "outbox_write=yes",
                "input_read=yes",
                "input_write=no",
                "snapshot_write=no",
                "git_write=no",
                "beside_write=no",
                "state_read=no",
                "global_read=no",
                "credential_read=no",
                "beside_read=no",
                "network=no",
            ],
            "{SCRIPT_PROFILE_MACOS} must allow exactly the granted paths"
        );
        for leak in [
            "state-secret",
            "global-secret",
            "ssh-secret",
            "beside-secret",
        ] {
            assert!(
                !String::from_utf8_lossy(&run.log).contains(leak),
                "{leak} escaped the profile"
            );
        }
        assert!(!beside.join("new").exists());
        assert!(!cwd.join(".git/new").exists());
        assert_eq!(
            std::fs::read_to_string(brief_dir.join("nested/brief.txt")).expect("brief"),
            "brief",
            "the script read a copy; the source is untouched"
        );
    }

    /// macOS gives a script no network: the probe refuses every network
    /// grant, the profile refuses to compile one, and a run asking for one
    /// is refused before anything starts.
    #[test]
    fn a_networked_script_is_refused_before_it_runs() {
        for network in [
            ScriptSandboxNetwork::TcpPort(443),
            ScriptSandboxNetwork::Any,
        ] {
            let refused =
                probe_script_sandbox_network(network).expect_err("macOS refuses a network grant");
            assert!(refused.contains("no network on macOS"), "{refused}");
        }
        let dir = tempfile::tempdir().expect("fixture");
        let root = dir.path().canonicalize().expect("physical fixture");
        let cwd = root.join("work");
        std::fs::create_dir_all(&cwd).expect("cwd");
        let scratch = scratch_in(&root);
        let marker = cwd.join("ran");
        let argv = vec!["/usr/bin/touch".to_string(), marker.display().to_string()];
        let sandbox = spec(
            &cwd,
            &scratch,
            &[],
            &[],
            &root,
            &[],
            ScriptSandboxNetwork::TcpPort(443),
        );
        let error = compile_macos_script_profile(&sandbox).expect_err("no networked profile");
        assert!(
            matches!(error, OrbitError::PolicyDenied(_)),
            "unexpected: {error:?}"
        );
        let error = run_script_sandboxed(request(sandbox, &argv, &[]))
            .expect_err("a networked run is refused");
        assert!(
            matches!(error, OrbitError::PolicyDenied(_)),
            "unexpected: {error:?}"
        );
        assert!(!marker.exists(), "a refused run must not start the script");
    }

    const NESTED_CHILD_ENV: &str = "ORBIT_TEST_SCRIPT_NESTED_CHILD";

    /// A script step started inside another Seatbelt sandbox (a worker's)
    /// refuses with the nested-sandbox reason and runs nothing, instead of
    /// nesting or running under the outer profile. Where this process can
    /// apply a profile, the check runs again in a child of this binary
    /// started under a permissive outer profile; where it cannot (an agent
    /// lane's own sandbox), this process is the nested start.
    #[test]
    fn a_nested_start_is_refused_and_runs_nothing() {
        let nested_here = matches!(
            orbit_exec::probe_sandbox_exec_apply(),
            orbit_exec::SandboxExecApplyProbe::ApplyRefused(_)
        );
        if nested_here || std::env::var_os(NESTED_CHILD_ENV).is_some() {
            assert_nested_start_refused();
            return;
        }
        let test = "script_sandbox::macos::a_nested_start_is_refused_and_runs_nothing";
        let sandbox_exec = orbit_exec::sandbox_exec_path().expect("sandbox-exec");
        let mut command = std::process::Command::new(sandbox_exec);
        command
            .args(["-p", "(version 1) (allow default)"])
            .arg(std::env::current_exe().expect("test binary"))
            .args(["--exact", test, "--nocapture", "--test-threads=1"])
            .env(NESTED_CHILD_ENV, "1")
            .stdin(std::process::Stdio::null());
        let output = orbit_common::process::run_bounded_capped(
            &mut command,
            Duration::from_secs(120),
            1024 * 1024,
        )
        .expect("nested child");
        orbit_common::test_env::assert_child_test_passed(
            test,
            output.status,
            output.stdout,
            output.stderr,
        );
    }

    fn assert_nested_start_refused() {
        let reason = probe_script_sandbox().expect_err("a nested start has no profile");
        assert!(
            reason.contains("already runs inside a Seatbelt sandbox")
                && reason.contains("does not start nested"),
            "{reason}"
        );
        let dir = tempfile::tempdir().expect("fixture");
        let root = dir.path().canonicalize().expect("physical fixture");
        let cwd = root.join("work");
        std::fs::create_dir_all(&cwd).expect("cwd");
        let scratch = scratch_in(&root);
        let marker = cwd.join("ran");
        let argv = vec!["/usr/bin/touch".to_string(), marker.display().to_string()];
        let error = run_script_sandboxed(request(
            spec(
                &cwd,
                &scratch,
                &[],
                &[],
                &root,
                &[],
                ScriptSandboxNetwork::None,
            ),
            &argv,
            &[],
        ))
        .expect_err("a nested run is refused");
        assert!(
            matches!(&error, OrbitError::PolicyDenied(message) if message.contains("does not start nested")),
            "unexpected: {error:?}"
        );
        assert!(!marker.exists(), "a nested start must run nothing");
    }
}
