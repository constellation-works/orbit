//! Build-phase supervision bounds (fault injection against a real process
//! group), the `fetch` phase's Landlock TCP rule, and the macOS build profile
//! under the real `sandbox-exec` (kernel behaviour).

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

    use crate::build_sandbox::{
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
        if !crate::macos_sandbox_test_guard("an_offline_phase_reaches_only_its_build_directory") {
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
mod supervise;
