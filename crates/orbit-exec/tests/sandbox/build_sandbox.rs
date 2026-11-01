//! Exported build phase and capped log boundaries.
use orbit_exec::BuildLog;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use orbit_exec::{
    BuildPhaseEnd, BuildPhaseNetwork, BuildPhaseRequest, BuildSandboxSpec, run_build_phase,
};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::time::Duration;

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn path_env() -> Vec<(String, String)> {
    vec![("PATH".to_string(), "/usr/bin:/bin".to_string())]
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn build_sandbox_available(test: &str) -> bool {
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

/// The phase sees only the environment it was given, and cannot dump core.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn a_phase_gets_only_its_environment_and_no_core_dumps() {
    if !build_sandbox_available("a_phase_gets_only_its_environment_and_no_core_dumps") {
        return;
    }
    if super::audit_support::run_isolated(
        "build_sandbox::a_phase_gets_only_its_environment_and_no_core_dumps",
    ) {
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let cwd_dir = tempfile::tempdir().expect("working directory");
    let cwd = cwd_dir
        .path()
        .canonicalize()
        .expect("physical working directory");
    let _ambient = orbit_common::test_env::scoped([("ORBIT_BUILD_AMBIENT_PROBE", Some("leaked"))]);
    let env = path_env();
    let mut log = BuildLog::default();
    let argv = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        "pwd; env; echo core=$(ulimit -c)".to_string(),
    ];
    let readable = vec![cwd.clone()];
    let build_dir = dir.path().canonicalize().expect("physical build directory");
    let request = BuildPhaseRequest {
        sandbox: BuildSandboxSpec {
            build_dir: &build_dir,
            readable: &readable,
            home: None,
            network: BuildPhaseNetwork::None,
        },
        argv: &argv,
        env: &env,
        cwd: &cwd,
        timeout: Duration::from_secs(20),
        build_dir_cap_bytes: u64::MAX,
    };
    let end = run_build_phase(&request, &mut log).expect("run");
    assert_eq!(end, BuildPhaseEnd::Exited(0));
    let output = String::from_utf8_lossy(&log.render()).into_owned();
    assert_eq!(
        output.lines().next(),
        Some(cwd.to_str().expect("utf8 cwd")),
        "a phase must honor its requested cwd"
    );
    assert!(!output.contains("ORBIT_BUILD_AMBIENT_PROBE"), "{output}");
    assert!(output.contains("core=0"), "{output}");
}

#[cfg(target_os = "macos")]
mod macos {
    //! `macos-sandbox-build-v1` against the real `sandbox-exec` (§3.2–§3.3): a
    //! networked phase is refused before it runs, and an offline phase reaches
    //! only its build directory and no network.
    //!
    //! A host where `sandbox-exec` cannot apply a profile (a nested sandbox)
    //! skips the kernel check. The macOS CI leg sets
    //! `ORBIT_REQUIRE_SANDBOX_EXEC=1`, which turns that skip into a failure.

    use std::path::Path;
    use std::time::Duration;

    use orbit_common::OrbitError;

    use orbit_exec::{
        BuildLog, BuildPhaseEnd, BuildPhaseNetwork, BuildPhaseRequest, BuildSandboxSpec,
        probe_build_sandbox, run_build_phase,
    };

    fn request<'a>(
        build_dir: &'a Path,
        network: BuildPhaseNetwork,
        argv: &'a [String],
        env: &'a [(String, String)],
    ) -> BuildPhaseRequest<'a> {
        BuildPhaseRequest {
            sandbox: BuildSandboxSpec {
                build_dir,
                readable: &[],
                home: None,
                network,
            },
            argv,
            env,
            cwd: build_dir,
            timeout: Duration::from_secs(30),
            build_dir_cap_bytes: u64::MAX,
        }
    }

    /// macOS has no pid namespace to take a `fetch` phase's escaped descendants
    /// down with it, so a networked phase is refused before any process starts.
    #[test]
    fn a_networked_phase_is_refused_before_it_runs() {
        assert!(
            probe_build_sandbox(true).is_err(),
            "macOS must not offer a profile for a fetch phase"
        );
        let dir = tempfile::tempdir().expect("build dir");
        let build_dir = dir.path().canonicalize().expect("physical build dir");
        let marker = build_dir.join("ran");
        let argv = vec!["/usr/bin/touch".to_string(), marker.display().to_string()];
        let env = Vec::new();

        let error = run_build_phase(
            &request(&build_dir, BuildPhaseNetwork::Https, &argv, &env),
            &mut BuildLog::default(),
        )
        .expect_err("a networked phase is refused on macOS");

        assert!(
            matches!(error, OrbitError::PluginBuildFetchUnsupported(_)),
            "unexpected refusal: {error:?}"
        );
        assert!(!marker.exists(), "a refused phase must not run");
    }

    /// An offline phase writes its build directory, and cannot read or write
    /// beside it or open a TCP connection, even to loopback.
    #[test]
    fn an_offline_phase_reaches_only_its_build_directory() {
        if !orbit_exec::macos_sandbox_test_guard(
            "an_offline_phase_reaches_only_its_build_directory",
        ) {
            return;
        }
        let dir = tempfile::tempdir().expect("build dir");
        let build_dir = dir.path().canonicalize().expect("physical build dir");
        let beside = tempfile::tempdir().expect("sibling dir");
        let beside = beside.path().canonicalize().expect("physical sibling dir");
        let secret = beside.join("secret");
        std::fs::write(&secret, "secret-content").expect("write secret");
        let outside = beside.join("outside");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
        let port = listener.local_addr().expect("listener addr").port();
        let script = format!(
            "echo built > out; \
             cat '{secret}' && echo READ_LEAK; \
             echo x > '{outside}' && echo WRITE_LEAK; \
             (exec 3<>/dev/tcp/127.0.0.1/{port}) && echo NET_LEAK; \
             echo finished",
            secret = secret.display(),
            outside = outside.display(),
        );
        let argv = vec!["/bin/bash".to_string(), "-c".to_string(), script];
        let env = vec![("PATH".to_string(), "/usr/bin:/bin".to_string())];
        let mut log = BuildLog::default();

        let end = run_build_phase(
            &request(&build_dir, BuildPhaseNetwork::None, &argv, &env),
            &mut log,
        )
        .expect("run the phase");

        let output = String::from_utf8_lossy(&log.render()).into_owned();
        assert_eq!(end, BuildPhaseEnd::Exited(0), "{output}");
        assert!(output.contains("finished"), "{output}");
        assert_eq!(
            std::fs::read_to_string(build_dir.join("out"))
                .ok()
                .as_deref(),
            Some("built\n"),
            "the build directory must be writable: {output}"
        );
        for leak in ["secret-content", "READ_LEAK", "WRITE_LEAK", "NET_LEAK"] {
            assert!(
                !output.contains(leak),
                "{leak} escaped the profile: {output}"
            );
        }
        assert!(
            !outside.exists(),
            "a write beside the build directory landed"
        );
    }
}

/// The host size walk cannot count an outside file reachable only through a
/// symlink. A zero cap exposes any such traversal through the public phase.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn build_directory_size_does_not_follow_symlinks() {
    if !build_sandbox_available("build_directory_size_does_not_follow_symlinks") {
        return;
    }
    let dir = tempfile::tempdir().expect("build dir");
    let outside = tempfile::tempdir().expect("outside dir");
    std::fs::write(outside.path().join("large"), [0u8; 16]).expect("outside file");
    std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).expect("symlink");
    let build_dir = dir.path().canonicalize().expect("physical build dir");
    let argv = vec!["/usr/bin/true".to_string()];
    let request = BuildPhaseRequest {
        sandbox: BuildSandboxSpec {
            build_dir: &build_dir,
            readable: &[],
            home: None,
            network: BuildPhaseNetwork::None,
        },
        argv: &argv,
        env: &[],
        cwd: &build_dir,
        timeout: Duration::from_secs(20),
        build_dir_cap_bytes: 0,
    };
    let end = run_build_phase(&request, &mut BuildLog::default()).expect("run the phase");
    assert_eq!(
        end,
        BuildPhaseEnd::Exited(0),
        "an outside file reachable only through a symlink is not part of the build directory"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn script_grants_cannot_override_state_or_git_protections() {
    use orbit_exec::{ScriptSandboxSpec, compile_linux_script_argv};
    use std::fs;

    let dir = tempfile::tempdir().expect("fixture");
    let cwd = dir.path().join("work");
    let state = cwd.join(".orbit");
    let global = dir.path().join("global");
    let output = dir.path().join("output");
    let scratch = dir.path().join("scratch");
    for path in [&cwd, &state, &global, &output, &scratch, &cwd.join(".git")] {
        fs::create_dir_all(path).expect("fixture directory");
    }
    let masked = vec![state.clone(), global.clone()];
    let writable = vec![dir.path().to_path_buf(), cwd.clone(), output.clone()];
    let spec = ScriptSandboxSpec {
        cwd: &cwd,
        cwd_writable: true,
        scratch: &scratch,
        readable: &[],
        writable: &writable,
        masked: &masked,
        home: None,
    };
    let args =
        compile_linux_script_argv(&spec, &[], &["/bin/true".into()]).expect("compile profile");
    let bind = |flag: &str, path: &std::path::Path| {
        let path = path.to_str().expect("path");
        args.windows(2)
            .rposition(|pair| pair[0] == flag && pair[1] == path)
            .expect("mount")
    };
    let last_grant = bind("--bind", &output)
        .max(bind("--bind", &cwd))
        .max(bind("--bind", dir.path()));
    for path in [&state, &global] {
        assert!(
            bind("--tmpfs", path) > last_grant,
            "state mask must win over every grant"
        );
    }
    assert!(
        bind("--ro-bind", &cwd.join(".git")) > last_grant,
        "Git metadata must stay read-only"
    );

    let enclosing_mask = vec![dir.path().to_path_buf()];
    let spec = ScriptSandboxSpec {
        masked: &enclosing_mask,
        ..spec
    };
    assert!(
        compile_linux_script_argv(&spec, &[], &["/bin/true".into()]).is_err(),
        "a grant must not expose an enclosing state tree"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn script_read_grants_cannot_expose_enclosing_state() {
    use orbit_exec::{ScriptSandboxSpec, compile_linux_script_argv};
    use std::fs;

    let dir = tempfile::tempdir().expect("fixture");
    let workspace = dir.path().join("workspace");
    let state = workspace.join(".orbit");
    let cwd = state.join("state/worktrees/run");
    let sibling = state.join("state/tasks");
    let other = dir.path().join("other");
    let scratch = dir.path().join("scratch");
    for path in [&cwd, &sibling, &other, &scratch] {
        fs::create_dir_all(path).expect("fixture directory");
    }
    let alias = dir.path().join("workspace-alias");
    std::os::unix::fs::symlink(&workspace, &alias).expect("alias");
    let masked = vec![state.clone()];
    let compile = |readable: &[std::path::PathBuf]| {
        let spec = ScriptSandboxSpec {
            cwd: &cwd,
            cwd_writable: true,
            scratch: &scratch,
            readable,
            writable: &[],
            masked: &masked,
            home: None,
        };
        compile_linux_script_argv(&spec, &[], &["/bin/true".into()])
    };
    for root in [&workspace, &alias, &state, &sibling] {
        assert!(
            compile(std::slice::from_ref(root)).is_err(),
            "read root {} must not expose the enclosing state tree",
            root.display()
        );
    }
    let args = compile(&[other.clone(), cwd.join("sub")]).expect("unrelated read root");
    let other = other.to_str().expect("path");
    assert!(
        args.windows(3)
            .any(|triple| triple[0] == "--ro-bind" && triple[1] == other && triple[2] == other),
        "an unrelated read root stays bound"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn script_profile_preserves_masks_with_overlapping_grant() {
    use orbit_exec::{ScriptRunEnd, ScriptSandboxRequest, ScriptSandboxSpec, run_script_sandboxed};
    use std::fs;

    if !build_sandbox_available("script_profile_preserves_masks_with_overlapping_grant") {
        return;
    }
    let dir = tempfile::tempdir().expect("fixture");
    let cwd = dir.path().join("work");
    let state = cwd.join(".orbit");
    let git = cwd.join(".git");
    let global = dir.path().join("global");
    let output = dir.path().join("output");
    let scratch = dir.path().join("scratch");
    for path in [&state, &git, &global, &output, &scratch] {
        fs::create_dir_all(path).expect("fixture directory");
    }
    for name in orbit_exec::SCRIPT_SCRATCH_DIRS {
        fs::create_dir_all(scratch.join(name)).expect("scratch directory");
    }
    fs::write(state.join("secret"), "secret").expect("state");
    fs::write(global.join("secret"), "secret").expect("global");
    let masked = vec![state.clone(), global.clone()];
    let writable = vec![dir.path().to_path_buf(), cwd.clone(), output.clone()];
    let script = format!(
        "test ! -e '{}/secret' && test ! -e '{}/secret' && ! touch '{}/new' && echo ok > '{}/result'",
        state.display(),
        global.display(),
        git.display(),
        output.display()
    );
    let argv = vec!["/bin/sh".into(), "-c".into(), script];
    let run = run_script_sandboxed(ScriptSandboxRequest {
        sandbox: ScriptSandboxSpec {
            cwd: &cwd,
            cwd_writable: true,
            scratch: &scratch,
            readable: &[],
            writable: &writable,
            masked: &masked,
            home: None,
        },
        argv: &argv,
        env: &[],
        launcher: &[],
        launcher_env: &[],
        stdin: vec![],
        timeout: Duration::from_secs(20),
        scratch_cap_bytes: u64::MAX,
    })
    .expect("run script");
    assert_eq!(
        run.end,
        ScriptRunEnd::Exited(0),
        "{}",
        String::from_utf8_lossy(&run.log)
    );
    assert_eq!(
        fs::read_to_string(output.join("result")).expect("output"),
        "ok\n"
    );
    assert!(!git.join("new").exists());
}

/// A script in a linked worktree reads the repository's Git metadata, which
/// lives in the common directory, and that directory is never writable.
#[cfg(target_os = "linux")]
mod script_linked_worktree {
    use super::build_sandbox_available;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use orbit_exec::{
        SCRIPT_SCRATCH_DIRS, ScriptRunEnd, ScriptSandboxRequest, ScriptSandboxSpec,
        compile_linux_script_argv, run_script_sandboxed,
    };

    struct LinkedWorktree {
        _temp: tempfile::TempDir,
        worktree: PathBuf,
        common: PathBuf,
        gitdir: PathBuf,
        scratch: PathBuf,
    }

    fn git(cwd: &Path, args: &[&str]) -> String {
        let output = crate::git_test_support::command()
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .current_dir(cwd)
            .args(["-c", "commit.gpgSign=false"])
            .args([
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.com",
            ])
            .args(args)
            .output()
            .expect("git");
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .expect("utf8 git output")
            .trim()
            .to_string()
    }

    fn linked_worktree() -> LinkedWorktree {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().canonicalize().expect("canonical root");
        let primary = root.join("primary");
        let worktree = root.join("worktree");
        let scratch = root.join("scratch");
        fs::create_dir(&primary).expect("primary");
        git(&primary, &["init", "-q"]);
        git(
            &primary,
            &["commit", "-q", "--allow-empty", "-m", "fixture"],
        );
        git(
            &primary,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "linked",
                worktree.to_str().expect("utf8 worktree path"),
            ],
        );
        for directory in SCRIPT_SCRATCH_DIRS {
            fs::create_dir_all(scratch.join(directory)).expect("scratch directory");
        }
        let common = primary
            .join(".git")
            .canonicalize()
            .expect("common directory");
        let gitdir = common.join("worktrees").join("worktree");
        LinkedWorktree {
            worktree: worktree.canonicalize().expect("worktree"),
            common,
            gitdir,
            scratch: scratch.canonicalize().expect("scratch"),
            _temp: temp,
        }
    }

    /// Where a read-only bind of `path` onto itself sits in the argv.
    fn ro_bind_at(argv: &[String], path: &Path) -> Option<usize> {
        let path = path.display().to_string();
        argv.windows(3)
            .position(|bind| bind[0] == "--ro-bind" && bind[1] == path && bind[2] == path)
    }

    #[test]
    fn the_common_git_directory_is_read_only_beneath_the_worktree_gitdir() {
        let fixture = linked_worktree();
        let spec = ScriptSandboxSpec {
            cwd: &fixture.worktree,
            cwd_writable: true,
            scratch: &fixture.scratch,
            readable: &[],
            writable: &[],
            masked: &[],
            home: None,
        };
        let argv = compile_linux_script_argv(&spec, &[], &["/usr/bin/true".to_string()])
            .expect("compile profile");
        let pointer_at = ro_bind_at(&argv, &fixture.worktree.join(".git"))
            .expect("the worktree Git pointer is bound read-only");
        let cwd = fixture.worktree.display().to_string();
        let cwd_at = argv
            .windows(3)
            .position(|bind| bind[0] == "--bind" && bind[1] == cwd && bind[2] == cwd)
            .expect("the worktree is bound writable");
        assert!(
            pointer_at > cwd_at,
            "the pointer bind must cover the writable worktree"
        );
        let common_at = ro_bind_at(&argv, &fixture.common)
            .expect("the common Git directory is bound read-only");
        let gitdir_at =
            ro_bind_at(&argv, &fixture.gitdir).expect("the per-worktree gitdir is bound read-only");
        assert!(
            common_at < gitdir_at,
            "the per-worktree gitdir must mount over the common directory: {argv:?}"
        );
        let worktree = fixture.worktree.display().to_string();
        assert!(
            argv.windows(3)
                .any(|bind| bind[0] == "--bind" && bind[1] == worktree && bind[2] == worktree),
            "the run worktree stays writable: {argv:?}"
        );
        let common = fixture.common.display().to_string();
        assert!(
            !argv
                .windows(3)
                .any(|bind| bind[0] == "--bind" && bind[1] == common),
            "the common Git directory is never writable: {argv:?}"
        );
    }

    #[test]
    fn a_script_in_a_linked_worktree_reads_git_and_keeps_its_writes() {
        if !build_sandbox_available("a_script_in_a_linked_worktree_reads_git_and_keeps_its_writes")
        {
            return;
        }
        let fixture = linked_worktree();
        let head = git(&fixture.worktree, &["rev-parse", "HEAD"]);
        let config = fixture.common.join("config");
        let config_before = fs::read(&config).expect("common config");
        let pointer_before = fs::read(fixture.worktree.join(".git")).expect("Git pointer");
        let script = format!(
            "set -eu\n\
             git rev-parse HEAD\n\
             git log -1 --format=%H\n\
             git status --porcelain\n\
             git archive --format=tar -o /orbit-script/out/archive.tar HEAD\n\
             echo generated > generated.txt\n\
             if (echo tampered >> '{}') 2>/dev/null; then exit 3; fi\n\
             if (: > .git) 2>/dev/null; then exit 4; fi\n\
             if rm .git 2>/dev/null; then exit 5; fi\n\
             if mv .git moved-git 2>/dev/null; then exit 6; fi\n\
             echo replacement > replacement\n\
             if mv -f replacement .git 2>/dev/null; then exit 7; fi\n\
             if ln -sf replacement .git 2>/dev/null; then exit 8; fi\n",
            config.display()
        );
        let argv = vec!["/bin/sh".to_string(), "-c".to_string(), script];
        let env = vec![
            ("PATH".to_string(), "/usr/bin:/bin".to_string()),
            ("HOME".to_string(), "/orbit-script/home".to_string()),
        ];
        let request = ScriptSandboxRequest {
            sandbox: ScriptSandboxSpec {
                cwd: &fixture.worktree,
                cwd_writable: true,
                scratch: &fixture.scratch,
                readable: &[],
                writable: &[],
                masked: &[],
                home: None,
            },
            argv: &argv,
            env: &env,
            launcher: &[],
            launcher_env: &[],
            stdin: Vec::new(),
            timeout: Duration::from_secs(60),
            scratch_cap_bytes: 64 * 1024 * 1024,
        };
        let run = run_script_sandboxed(request).expect("run the script");
        assert_eq!(
            run.end,
            ScriptRunEnd::Exited(0),
            "log: {}",
            String::from_utf8_lossy(&run.log)
        );
        assert_eq!(
            String::from_utf8_lossy(&run.stdout),
            format!("{head}\n{head}\n")
        );
        assert!(fixture.scratch.join("out/archive.tar").is_file());
        assert_eq!(
            fs::read(fixture.worktree.join(".git")).expect("Git pointer"),
            pointer_before
        );
        assert_eq!(
            fs::read_to_string(fixture.worktree.join("generated.txt")).expect("source write"),
            "generated\n"
        );
        assert_eq!(
            fs::read(&config).expect("common config"),
            config_before,
            "the common Git config is unchanged"
        );
    }
}

/// The failing shape of a granted script whose read root is a file under the
/// masked global root (`~/.orbit/bin/orbit`): the mask must neither hide the
/// file nor leave the credential deny for `~/.orbit/run` nowhere to mount.
#[cfg(target_os = "linux")]
#[test]
fn script_read_root_under_masked_global_root_starts_and_keeps_state_hidden() {
    use orbit_exec::{ScriptRunEnd, ScriptSandboxRequest, ScriptSandboxSpec, run_script_sandboxed};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().expect("fixture");
    let home = dir.path().join("home");
    let global = home.join(".orbit");
    let tool = global.join("bin/orbit");
    let cwd = dir.path().join("work");
    let scratch = dir.path().join("scratch");
    for path in [
        &global.join("bin"),
        &global.join("run"),
        &global.join("state"),
        &cwd,
        &scratch,
    ] {
        fs::create_dir_all(path).expect("fixture directory");
    }
    for name in orbit_exec::SCRIPT_SCRATCH_DIRS {
        fs::create_dir_all(scratch.join(name)).expect("scratch directory");
    }
    fs::write(&tool, "#!/bin/sh\necho tool-ran\n").expect("tool");
    fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).expect("tool mode");
    fs::write(global.join("state/secret"), "secret").expect("state");
    fs::write(global.join("run/web-session.secret"), "secret").expect("run");
    let masked = vec![global.clone()];
    let readable = vec![tool.clone()];
    let spec = ScriptSandboxSpec {
        cwd: &cwd,
        cwd_writable: false,
        scratch: &scratch,
        readable: &readable,
        writable: &[],
        masked: &masked,
        home: Some(home.as_os_str()),
    };

    let args = compile_args(&spec);
    let position = |flag: &str, path: &std::path::Path| {
        let path = path.to_str().expect("path");
        args.windows(2)
            .rposition(|pair| pair[0] == flag && pair[1] == path)
            .unwrap_or_else(|| panic!("no `{flag} {path}`"))
    };
    let run_deny = position("--tmpfs", &global.join("run"));
    assert!(
        position("--tmpfs", &global) < run_deny,
        "the credential deny mounts inside the mask"
    );
    assert!(
        run_deny < position("--remount-ro", &global),
        "the mask goes read-only only after the credential denies"
    );
    assert!(
        position("--tmpfs", &global) < position("--ro-bind", &tool),
        "a read root inside the mask is bound over it"
    );

    if !build_sandbox_available("script_read_root_under_masked_global_root_starts") {
        return;
    }
    let script = format!(
        "'{}' && test ! -e '{}/state/secret' && test ! -e '{}/run/web-session.secret'",
        tool.display(),
        global.display(),
        global.display()
    );
    let argv = vec!["/bin/sh".into(), "-c".into(), script];
    let run = run_script_sandboxed(ScriptSandboxRequest {
        sandbox: spec,
        argv: &argv,
        env: &[],
        launcher: &[],
        launcher_env: &[],
        stdin: vec![],
        timeout: Duration::from_secs(20),
        scratch_cap_bytes: u64::MAX,
    })
    .expect("run script");
    assert_eq!(
        run.end,
        ScriptRunEnd::Exited(0),
        "{}",
        String::from_utf8_lossy(&run.log)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "tool-ran\n");

    fn compile_args(spec: &ScriptSandboxSpec<'_>) -> Vec<String> {
        orbit_exec::compile_linux_script_argv(spec, &[], &["/bin/true".into()])
            .expect("compile profile")
    }
}
