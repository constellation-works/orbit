#![allow(missing_docs)]

use std::ffi::OsString;
#[cfg(target_os = "linux")]
use std::fs::File;
#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;

#[cfg(target_os = "linux")]
use super::super::spawn::linux_bwrap_mount_authority;
use orbit_exec::BwrapProbeOutcome;
#[cfg(target_os = "linux")]
use orbit_exec::{LinuxBwrapMountAuthority, compile_linux_bwrap_argv_with_authority, probe_bwrap};
use orbit_types::workflow::ExecutorSandboxKind;
use tempfile::tempdir;

use super::super::super::dispatcher::ResolvedSandbox;
use super::super::spawn::{
    SpawnError, SpawnedChild, prepare_linux_sandbox_for_dispatch_with_probe,
    prepare_macos_codex_ca_environment_with, reject_unsatisfiable_managed_grants, spawn_bare,
    spawn_macos_sandboxed_with,
};
use super::test_support::{linux_sandbox_for_test, sandbox_for_test, sh_args};

/// [ORB-10917] The bare launcher must hand the child exactly the environment
/// the dispatcher composed. The ambient variables below are set by this test
/// rather than read from the developer's shell, and none carries a
/// credential-shaped name — a denylist would forward every one of them.
#[cfg(unix)]
#[test]
fn spawn_bare_gives_the_child_only_the_supplied_environment() {
    let _ambient = orbit_common::test_env::scoped([
        ("DATABASE_URL", Some("postgres://svc:hunter2@db.internal")),
        ("BILLING_ENDPOINT", Some("https://billing.internal.example")),
        ("ORB_10917_AMBIENT", Some("leaked")),
        ("ANTHROPIC_API_KEY", Some("sk-ant-000000000000000000000")),
    ]);
    let env = vec![
        ("PATH".to_string(), "/usr/bin:/bin".to_string()),
        ("ORB_10917_SUPPLIED".to_string(), "present".to_string()),
    ];

    let spawned = spawn_bare("/bin/sh", &sh_args("env"), &env, None).expect("spawn bare child");
    let output = spawned
        .child
        .wait_with_output()
        .expect("wait for bare child");
    let child_env = String::from_utf8_lossy(&output.stdout);

    assert!(
        child_env.contains("ORB_10917_SUPPLIED=present"),
        "supplied vars must reach the child: {child_env}"
    );
    for leaked in [
        "DATABASE_URL",
        "BILLING_ENDPOINT",
        "ORB_10917_AMBIENT",
        "ANTHROPIC_API_KEY",
    ] {
        assert!(
            !child_env.contains(leaked),
            "{leaked} must not reach a bare-exec provider child: {child_env}"
        );
    }
}

fn env_value<'a>(env: &'a [(String, String)], name: &str) -> Option<&'a str> {
    env.iter()
        .rev()
        .find(|(candidate, _)| candidate == name)
        .map(|(_, value)| value.as_str())
}

#[test]
fn macos_codex_ca_environment_supplies_the_readable_public_bundle_by_default() {
    let fixture = tempdir().expect("tempdir");
    let bundle = fixture.path().join("public-ca.pem");
    std::fs::write(
        &bundle,
        "-----BEGIN CERTIFICATE-----\nfixture\n-----END CERTIFICATE-----\n",
    )
    .expect("write CA fixture");
    let input = vec![("HOME".to_string(), "/Users/test".to_string())];

    let prepared = prepare_macos_codex_ca_environment_with("codex", &input, None, &bundle)
        .expect("readable public CA bundle");

    assert_eq!(
        env_value(&prepared, "CODEX_CA_CERTIFICATE"),
        Some(bundle.to_string_lossy().as_ref())
    );
    assert_eq!(env_value(&prepared, "SSL_CERT_FILE"), None);
    assert_eq!(env_value(&prepared, "HOME"), Some("/Users/test"));
}

#[test]
fn macos_codex_ca_environment_preserves_explicit_override_precedence() {
    let fixture = tempdir().expect("tempdir");
    let codex_bundle = fixture.path().join("codex.pem");
    let ssl_bundle = fixture.path().join("ssl.pem");
    let default_bundle = fixture.path().join("default.pem");
    for path in [&codex_bundle, &ssl_bundle, &default_bundle] {
        std::fs::write(path, "fixture").expect("write CA fixture");
    }
    let env = vec![
        (
            "SSL_CERT_FILE".to_string(),
            ssl_bundle.to_string_lossy().into_owned(),
        ),
        (
            "CODEX_CA_CERTIFICATE".to_string(),
            codex_bundle.to_string_lossy().into_owned(),
        ),
    ];

    let prepared = prepare_macos_codex_ca_environment_with("codex", &env, None, &default_bundle)
        .expect("explicit Codex CA wins");

    assert_eq!(
        prepared, env,
        "an explicit environment must not be rewritten"
    );
    assert_eq!(
        env_value(&prepared, "CODEX_CA_CERTIFICATE"),
        Some(codex_bundle.to_string_lossy().as_ref())
    );
    assert_eq!(
        env_value(&prepared, "SSL_CERT_FILE"),
        Some(ssl_bundle.to_string_lossy().as_ref())
    );
}

#[test]
fn macos_codex_ca_environment_uses_ssl_cert_file_without_injecting_codex_override() {
    let fixture = tempdir().expect("tempdir");
    let ssl_bundle = fixture.path().join("ssl.pem");
    std::fs::write(&ssl_bundle, "fixture").expect("write CA fixture");
    let env = vec![(
        "SSL_CERT_FILE".to_string(),
        ssl_bundle.to_string_lossy().into_owned(),
    )];

    let prepared = prepare_macos_codex_ca_environment_with(
        "codex",
        &env,
        None,
        &fixture.path().join("unused-default.pem"),
    )
    .expect("explicit SSL bundle wins over Orbit default");

    assert_eq!(prepared, env);
    assert_eq!(env_value(&prepared, "CODEX_CA_CERTIFICATE"), None);
}

#[test]
fn macos_codex_ca_environment_rejects_selected_missing_material() {
    let fixture = tempdir().expect("tempdir");
    let default_bundle = fixture.path().join("default.pem");
    std::fs::write(&default_bundle, "fixture").expect("write CA fixture");

    let env = vec![(
        "CODEX_CA_CERTIFICATE".to_string(),
        fixture
            .path()
            .join("missing-explicit.pem")
            .to_string_lossy()
            .into_owned(),
    )];
    let error = prepare_macos_codex_ca_environment_with("codex", &env, None, &default_bundle)
        .expect_err("invalid explicit material must not fall back");

    assert!(error.permanent);
    assert!(error.message.contains("CODEX_CA_CERTIFICATE"));
    assert!(error.message.contains("readable PEM CA bundle"));

    let error = prepare_macos_codex_ca_environment_with(
        "codex",
        &[],
        None,
        &fixture.path().join("missing-default.pem"),
    )
    .expect_err("a missing Orbit default must fail before provider launch");
    assert!(error.permanent);
    assert!(error.message.contains("CODEX_CA_CERTIFICATE"));
    assert!(error.message.contains("missing-default.pem"));
}

#[test]
fn macos_codex_ca_environment_treats_empty_overrides_as_unset() {
    let fixture = tempdir().expect("tempdir");
    let ssl_bundle = fixture.path().join("ssl.pem");
    let default_bundle = fixture.path().join("default.pem");
    std::fs::write(&ssl_bundle, "fixture").expect("write SSL fixture");
    std::fs::write(&default_bundle, "fixture").expect("write default fixture");
    let env = vec![
        ("CODEX_CA_CERTIFICATE".to_string(), String::new()),
        (
            "SSL_CERT_FILE".to_string(),
            ssl_bundle.to_string_lossy().into_owned(),
        ),
    ];

    let prepared = prepare_macos_codex_ca_environment_with("codex", &env, None, &default_bundle)
        .expect("empty Codex override falls back to explicit SSL_CERT_FILE");

    assert_eq!(prepared, env);

    let empty_env = vec![
        ("CODEX_CA_CERTIFICATE".to_string(), String::new()),
        ("SSL_CERT_FILE".to_string(), String::new()),
    ];
    let prepared =
        prepare_macos_codex_ca_environment_with("codex", &empty_env, None, &default_bundle)
            .expect("empty overrides fall back to Orbit public bundle");
    assert_eq!(
        env_value(&prepared, "CODEX_CA_CERTIFICATE"),
        Some(default_bundle.to_string_lossy().as_ref())
    );
}

#[test]
fn macos_codex_ca_environment_does_not_change_other_providers() {
    let env = vec![(
        "CODEX_CA_CERTIFICATE".to_string(),
        "/missing/operator/value.pem".to_string(),
    )];

    let prepared = prepare_macos_codex_ca_environment_with(
        "claude",
        &env,
        None,
        std::path::Path::new("/missing/default.pem"),
    )
    .expect("Claude CA handling is unchanged");

    assert_eq!(prepared, env);
}

/// [ORB-10879] Regression guard for the managed-worktree pre-spawn check. Its
/// whole purpose is that an unsatisfiable grant fails *before* the provider
/// starts rather than surfacing as an EROFS mid-turn, so the rejection is
/// asserted directly instead of through a spawn that would skip without bwrap.
#[test]
fn unsatisfiable_grant_in_a_managed_worktree_fails_before_the_provider_starts() {
    let dropped = vec![orbit_exec::UnsatisfiedWriteGrant {
        rule: "/tmp/orbit-jrun-attribution/.orbit/routines/**".to_string(),
        anchor: std::path::PathBuf::from("/tmp/orbit-jrun-attribution/.orbit/routines"),
        reason: "anchor is absent".to_string(),
    }];

    let error = reject_unsatisfiable_managed_grants(true, &dropped)
        .expect_err("an unsatisfiable grant must not reach the provider");

    assert!(
        error.permanent,
        "an unmountable grant set is deterministic config, not a transient fault"
    );
    assert!(
        error.message.contains(".orbit/routines"),
        "rejection must name the grant that could not be applied: {}",
        error.message
    );
    assert!(
        error
            .message
            .contains("could not apply 1 policy write grant"),
        "rejection must count the dropped grants: {}",
        error.message
    );
}

/// Outside a managed worktree the same grant is host-owned, so it is reported
/// rather than fatal — widening this check would fail runs the host can fix.
#[test]
fn unsatisfiable_grant_outside_a_managed_worktree_is_not_fatal() {
    let dropped = vec![orbit_exec::UnsatisfiedWriteGrant {
        rule: "/var/host-owned/**".to_string(),
        anchor: std::path::PathBuf::from("/var/host-owned"),
        reason: "anchor is absent".to_string(),
    }];

    reject_unsatisfiable_managed_grants(false, &dropped)
        .expect("host-owned anchors stay reported, not fatal");
    reject_unsatisfiable_managed_grants(true, &[]).expect("a fully satisfied grant set must spawn");
}

#[test]
fn spawn_bare_runs_program_in_provided_cwd() {
    let temp = tempdir().expect("tempdir");
    let cwd = temp.path().canonicalize().expect("canonical tempdir");
    let SpawnedChild {
        child,
        _profile_temp,
        _linux_mount_plan,
    } = spawn_bare("/bin/sh", &sh_args("pwd"), &[], Some(&cwd)).expect("spawn succeeds");

    let output = child.wait_with_output().expect("wait succeeds");
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).expect("stdout utf8"),
        format!("{}\n", cwd.display())
    );
}

#[test]
fn spawn_bare_does_not_inherit_ambient_sensitive_env() {
    let _guard = EnvVarGuard::set("ORBIT_SPAWN_BARE_TEST_TOKEN", "parent-process-secret-value");
    let SpawnedChild {
        child,
        _profile_temp,
        _linux_mount_plan,
    } = spawn_bare(
        "/bin/sh",
        &sh_args(
            "if [ -z \"${ORBIT_SPAWN_BARE_TEST_TOKEN+x}\" ]; then printf unset; else printf 'leaked:%s' \"$ORBIT_SPAWN_BARE_TEST_TOKEN\"; fi",
        ),
        &[],
        None,
    )
    .expect("spawn succeeds");

    let output = child.wait_with_output().expect("wait succeeds");
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).expect("stdout utf8"),
        "unset"
    );
}

/// Regression for the engine ownership boundary behind the managed SQLite
/// CannotOpen failure. The descriptor plan must remain alive after the child
/// exits and until the supervisor drops the complete spawned-child guard.
#[cfg(target_os = "linux")]
#[test]
fn spawned_child_guard_retains_linux_mount_descriptors() {
    let temp = tempdir().expect("tempdir");
    let root = temp.path().canonicalize().expect("canonical root");
    let target = root.join("orbit.db");
    std::fs::write(&target, b"sqlite-object").expect("runtime object");
    let profile = orbit_types::policy::ResolvedFsProfile {
        name: "test".to_string(),
        read: vec!["/**".to_string()],
        modify: vec![target.display().to_string()],
    };
    let source =
        std::sync::Arc::new(File::open(root.join("orbit.db")).expect("descriptor authority"));
    let authority_fd = source.as_raw_fd();
    let plan = compile_linux_bwrap_argv_with_authority(
        &profile,
        "/bin/true",
        &[],
        Some(&root),
        false,
        vec![LinuxBwrapMountAuthority {
            destination: target,
            source,
        }],
    )
    .expect("descriptor plan");
    let source_fd = plan.mount_evidence()[0].source_fd;
    assert_eq!(
        source_fd, authority_fd,
        "plan compilation must share the authority handle, not duplicate its raw descriptor"
    );
    let SpawnedChild {
        child,
        _profile_temp,
        _linux_mount_plan: _,
    } = spawn_bare("/bin/sh", &sh_args("exit 0"), &[], Some(&root)).expect("child");
    let mut spawned = SpawnedChild {
        child,
        _profile_temp,
        _linux_mount_plan: Some(plan),
    };

    assert!(unsafe { libc::fcntl(source_fd, libc::F_GETFD) } >= 0);
    assert!(spawned.child.wait().expect("wait").success());
    assert!(
        unsafe { libc::fcntl(source_fd, libc::F_GETFD) } >= 0,
        "mount descriptor must survive child exit until supervision completes"
    );

    let authority_path = std::fs::canonicalize(root.join("orbit.db")).expect("authority path");
    drop(spawned);
    // A parallel test can reuse the descriptor number as soon as it closes, so
    // assert the number no longer names the authority, not that it is unused.
    let still_names_authority = std::fs::read_link(format!("/proc/self/fd/{source_fd}"))
        .is_ok_and(|path| path == authority_path);
    assert!(
        !still_names_authority,
        "dropping supervision must release the mount descriptor"
    );
}

/// Negative control for the proven fault: translating host runtime authority
/// into mount authority must share the same raw descriptor, never `dup` it.
#[cfg(target_os = "linux")]
#[test]
fn linux_runtime_mount_authority_shares_host_descriptor() {
    let temp = tempdir().expect("tempdir");
    let target = temp.path().join("orbit.db");
    std::fs::write(&target, b"sqlite-object").expect("runtime object");
    let source = std::sync::Arc::new(File::open(&target).expect("descriptor authority"));
    let source_fd = source.as_raw_fd();
    let sandbox = ResolvedSandbox {
        kind: ExecutorSandboxKind::LinuxBwrap,
        fs_profile: orbit_types::policy::ResolvedFsProfile {
            name: "test".to_string(),
            read: vec!["/**".to_string()],
            modify: vec![target.display().to_string()],
        },
        allow_fallback: false,
        managed_worktree: false,
        runtime_write_authority: vec![
            super::super::super::dispatcher::LinuxRuntimeWriteAuthority {
                path: target,
                handle: source,
                wal_file_set_lease: None,
            },
        ],
    };

    let authority = linux_bwrap_mount_authority(&sandbox);
    assert_eq!(authority.len(), 1);
    assert_eq!(authority[0].source.as_raw_fd(), source_fd);
    assert_eq!(std::sync::Arc::strong_count(&authority[0].source), 2);
}

/// Real-kernel counterpart: when Bubblewrap is available, exercise the actual
/// engine spawn path and require it to return ownership of the descriptor plan.
#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires a Linux host with working Bubblewrap user/mount namespaces"]
fn linux_bwrap_spawn_returns_mount_descriptor_ownership() {
    let probe = probe_bwrap();
    assert!(
        probe.available,
        "live boundary unvalidated: {}",
        probe.detail
    );
    let temp = tempdir().expect("tempdir");
    let root = temp.path().canonicalize().expect("canonical root");
    let target = root.join("orbit.db");
    std::fs::write(&target, b"sqlite-object").expect("runtime object");
    let sandbox = ResolvedSandbox {
        kind: ExecutorSandboxKind::LinuxBwrap,
        fs_profile: orbit_types::policy::ResolvedFsProfile {
            name: "test".to_string(),
            read: vec!["/**".to_string()],
            modify: vec![target.display().to_string()],
        },
        allow_fallback: false,
        managed_worktree: false,
        runtime_write_authority: vec![
            super::super::super::dispatcher::LinuxRuntimeWriteAuthority {
                path: target.clone(),
                handle: std::sync::Arc::new(File::open(&target).expect("descriptor authority")),
                wal_file_set_lease: None,
            },
        ],
    };
    let mut spawned = super::super::spawn::spawn_child_with_optional_sandbox(
        "/bin/sh",
        &sh_args("exit 0"),
        &[],
        Some(&root),
        Some(&sandbox),
        "codex",
    )
    .expect("sandboxed spawn");

    assert!(spawned._linux_mount_plan.is_some());
    assert_eq!(
        spawned
            ._linux_mount_plan
            .as_ref()
            .expect("retained plan")
            .mount_evidence()[0]
            .source_fd,
        sandbox.runtime_write_authority[0].handle.as_raw_fd(),
        "engine spawn must not create a parent-side duplicate descriptor"
    );
    assert!(spawned.child.wait().expect("wait").success());
}

#[test]
fn spawn_macos_sandboxed_returns_error_when_sandbox_exec_missing_and_fallback_disabled() {
    let sandbox = sandbox_for_test();
    let err = spawn_macos_sandboxed_with("/bin/sh", &[], &[], None, &sandbox, "claude", false)
        .expect_err("expected fallback-disabled error");
    assert!(
        err.permanent,
        "missing sandbox-exec is deterministic and must classify permanent"
    );
    assert!(
        err.message
            .contains("trusted sandbox-exec not available at /usr/bin/sandbox-exec"),
        "unexpected error message: {}",
        err.message
    );
    assert!(
        err.message.contains("allow_fallback: true"),
        "error should describe fallback opt-in: {}",
        err.message
    );
}

fn failed_bwrap_probe() -> BwrapProbeOutcome {
    BwrapProbeOutcome {
        available: false,
        trusted_path: "/usr/bin/bwrap".to_string(),
        detail: "Bubblewrap capability probe failed: user namespaces disabled".to_string(),
    }
}

#[test]
fn linux_bwrap_probe_failure_is_permanent_when_fallback_disabled() {
    let sandbox = linux_sandbox_for_test(false);
    let error = prepare_linux_sandbox_for_dispatch_with_probe(&sandbox, failed_bwrap_probe())
        .err()
        .expect("probe failure must stop dispatch");
    assert!(error.permanent);
    assert!(error.message.contains("user namespaces disabled"));
    assert!(error.message.contains("allow_fallback: true"));
}

#[test]
fn linux_bwrap_probe_failure_uses_honest_bare_fallback_metadata() {
    let sandbox = linux_sandbox_for_test(true);
    let prepared = prepare_linux_sandbox_for_dispatch_with_probe(&sandbox, failed_bwrap_probe())
        .expect("explicit fallback");
    assert!(prepared.effective.is_none());
    assert_eq!(prepared.metadata.backend.as_deref(), Some("bare-fallback"));
    assert_eq!(prepared.metadata.write_enforcement, "write_delegated");
    assert_eq!(prepared.metadata.read_enforcement, "read_delegated");
    assert_eq!(
        prepared.metadata.trusted_wrapper.as_deref(),
        Some("/usr/bin/bwrap")
    );
}

#[test]
fn successful_linux_bwrap_probe_marks_write_enforcement() {
    let sandbox = linux_sandbox_for_test(false);
    let prepared = prepare_linux_sandbox_for_dispatch_with_probe(
        &sandbox,
        BwrapProbeOutcome {
            available: true,
            trusted_path: "/usr/bin/bwrap".to_string(),
            detail: "capability probe succeeded".to_string(),
        },
    )
    .expect("probe success");
    assert!(prepared.effective.is_some());
    assert_eq!(prepared.metadata.backend.as_deref(), Some("linux-bwrap"));
    assert_eq!(prepared.metadata.write_enforcement, "write_enforced");
}

#[test]
fn spawn_bare_missing_executable_classifies_permanent() {
    let err = spawn_bare("/nonexistent/orbit-test-program", &[], &[], None)
        .expect_err("missing executable must fail");
    assert!(
        err.permanent,
        "ENOENT is deterministic and must classify permanent: {}",
        err.message
    );
    assert!(
        err.message.contains("/nonexistent/orbit-test-program"),
        "error should name the program: {}",
        err.message
    );
}

#[test]
fn spawn_io_error_classification_table() {
    use std::io::{Error, ErrorKind};
    // (kind, expected permanent) — clearly-deterministic failures fail fast;
    // resource exhaustion and everything unrecognized stays retryable.
    let table = [
        (ErrorKind::NotFound, true),
        (ErrorKind::PermissionDenied, true),
        (ErrorKind::WouldBlock, false),         // EAGAIN
        (ErrorKind::OutOfMemory, false),        // ENOMEM
        (ErrorKind::Interrupted, false),        // EINTR
        (ErrorKind::ExecutableFileBusy, false), // ETXTBSY: classified transient; spawn_bare does not retry
        (ErrorKind::Other, false),              // unknown → conservative: retryable
    ];
    for (kind, expect_permanent) in table {
        let classified = SpawnError::from_spawn_io("prog", &Error::new(kind, "boom"));
        assert_eq!(
            classified.permanent, expect_permanent,
            "kind {kind:?} misclassified (permanent={})",
            classified.permanent
        );
    }
}

#[test]
fn spawn_macos_sandboxed_falls_back_to_bare_exec_when_allow_fallback_set() {
    let sandbox = ResolvedSandbox {
        allow_fallback: true,
        ..sandbox_for_test()
    };
    let mut spawned = spawn_macos_sandboxed_with(
        "/bin/sh",
        &["-c".to_string(), "exit 0".to_string()],
        &[],
        None,
        &sandbox,
        "claude",
        false,
    )
    .expect("fallback should succeed");
    // The fallback path returns a SpawnedChild with no profile tempfile
    // because the sandbox-exec wrapper was bypassed.
    assert!(spawned._profile_temp.is_none());
    assert!(spawned._linux_mount_plan.is_none());
    let _ = spawned.child.wait();
}

struct EnvVarGuard {
    key: &'static str,
    previous: Option<OsString>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var_os(key);
        // SAFETY: this test uses a dedicated variable name and restores the
        // previous value on drop.
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        // SAFETY: see EnvVarGuard::set.
        unsafe {
            match &self.previous {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }
}
