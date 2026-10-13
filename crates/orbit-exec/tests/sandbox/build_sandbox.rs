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
