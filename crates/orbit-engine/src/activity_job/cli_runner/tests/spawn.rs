#![allow(missing_docs)]

use std::ffi::OsString;

use orbit_exec::BwrapProbeOutcome;
use orbit_types::workflow::ExecutorSandboxKind;
use tempfile::tempdir;

use super::super::super::dispatcher::ResolvedSandbox;
use super::super::spawn::{
    SUPPORTED_SYSTEM_BIN_DIRS, SpawnError, SpawnedChild, linux_bwrap_failed_write_diagnostic,
    macos_keychain_auth_diagnostic_with, orbit_tool_env_with,
    prepare_linux_sandbox_for_dispatch_with_probe, prepare_macos_codex_ca_environment_with,
    reject_unsatisfiable_managed_grants, resolve_provider_launcher_with,
    resolve_provider_launcher_with_extra_dirs, spawn_bare, spawn_macos_sandboxed_with,
};
use super::test_support::{sandbox_for_test, sh_args, write_executable};

/// A profile shaped like a managed-worktree implementer: the worktree is
/// writable, its `.orbit` store is not.
fn worktree_profile(worktree: &std::path::Path) -> orbit_types::policy::ResolvedFsProfile {
    orbit_types::policy::ResolvedFsProfile {
        name: "unrestricted".to_string(),
        read: vec![format!("{}/**", worktree.display())],
        modify: vec![
            format!("{}/**", worktree.display()),
            format!("!{}/.orbit/**", worktree.display()),
        ],
    }
}

/// [ORB-10879] The attribution path an operator actually depends on, exercised
/// without Bubblewrap and without consulting the host's mount table: a child
/// that reported EROFS gets a denial naming the attempted path and the rule
/// that shadowed it.
#[test]
fn failed_write_diagnostic_names_attempted_path_and_shadowing_rule() {
    let worktree = std::path::Path::new("/tmp/orbit-jrun-attribution");
    let profile = worktree_profile(worktree);
    let stderr = b"touch: cannot touch '/tmp/orbit-jrun-attribution/.orbit/state/x': Read-only file system\n";

    let diagnostic = linux_bwrap_failed_write_diagnostic(&profile, stderr, Some(worktree))
        .expect("diagnostic derivation succeeds")
        .expect("a denied write must be attributable");

    assert!(
        diagnostic.contains("/tmp/orbit-jrun-attribution/.orbit/state/x"),
        "diagnostic must name the attempted path: {diagnostic}"
    );
    assert!(
        diagnostic.contains("denyModify rule"),
        "diagnostic must name the shadowing rule: {diagnostic}"
    );
    assert!(
        diagnostic.contains(&format!("!{}/.orbit/**", worktree.display())),
        "diagnostic must quote the exact deny that shadows the path: {diagnostic}"
    );
    // The wrapper prefix is Orbit's, but the explanation body is verbatim
    // `linux_bwrap_write_grant_diagnostic` output — one message format.
    let expected =
        orbit_exec::linux_bwrap_write_grant_diagnostic(&profile, &worktree.join(".orbit/state/x"))
            .expect("grant diagnostic")
            .expect("path is denied");
    assert!(
        diagnostic.ends_with(&expected),
        "diagnostic must reuse the existing grant-diagnostic text: {diagnostic}"
    );
}

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

/// A path the profile *does* grant is not reported as a denial, so the
/// diagnostic cannot manufacture an attribution for an unrelated EROFS.
#[test]
fn failed_write_diagnostic_stays_silent_for_a_granted_path() {
    let worktree = std::path::Path::new("/tmp/orbit-jrun-attribution");
    let profile = worktree_profile(worktree);
    let stderr =
        b"touch: cannot touch '/tmp/orbit-jrun-attribution/docs/x.md': Read-only file system\n";

    let diagnostic = linux_bwrap_failed_write_diagnostic(&profile, stderr, Some(worktree))
        .expect("diagnostic derivation succeeds");

    assert!(
        diagnostic.is_none(),
        "a granted path must not be attributed to policy: {diagnostic:?}"
    );
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

/// The failure an operator actually sees when a Keychain-backed login cannot be
/// read: the provider says "expired", which is indistinguishable from a sandbox
/// denial unless Orbit attributes it. Both branches must be reachable, because
/// they call for different fixes.
#[test]
fn keychain_auth_diagnostic_separates_a_real_expiry_from_a_sandbox_denial() {
    let sandbox = sandbox_for_test();
    let failure = "Failed to authenticate: OAuth session expired and could not be refreshed";

    let with_home = macos_keychain_auth_diagnostic_with(
        "claude",
        Some(&sandbox),
        failure,
        Some(std::ffi::OsStr::new("/Users/test")),
    )
    .expect("claude under sandbox-exec must be attributed");
    assert!(
        with_home.contains("real login failure"),
        "a reachable keychain means re-authentication is the fix: {with_home}"
    );

    let without_home = macos_keychain_auth_diagnostic_with("claude", Some(&sandbox), failure, None)
        .expect("a HOME-less compile cannot emit the keychain allow");
    assert!(
        without_home.contains("HOME is unset"),
        "an unemitted keychain allow means re-authentication cannot help: {without_home}"
    );
}

/// [ORB-10931] The third case: the operator's own `denyRead` outranks the
/// provider carve-out, so Orbit — not the provider — hid the credential.
/// Recommending re-authentication here sends the operator to a fix that cannot
/// work, so the message must name the rule instead.
#[test]
fn keychain_auth_diagnostic_attributes_an_activity_deny_instead_of_a_relogin() {
    let failure = "Failed to authenticate: OAuth session expired and could not be refreshed";
    let home = std::ffi::OsStr::new("/Users/test");

    for deny in ["!/Users/test/Library/Keychains", "!/Users/test/Library"] {
        let mut sandbox = sandbox_for_test();
        sandbox.fs_profile.name = "hardened".to_string();
        sandbox.fs_profile.read.push(deny.to_string());

        let diagnostic =
            macos_keychain_auth_diagnostic_with("claude", Some(&sandbox), failure, Some(home))
                .expect("an activity keychain deny must be attributed");

        assert!(
            diagnostic.contains(deny) && diagnostic.contains("hardened"),
            "diagnostic must name the profile and the exact deny rule: {diagnostic}"
        );
        assert!(
            diagnostic.contains("Re-authenticating will not help"),
            "an Orbit-authored denial must not send the operator to re-login: {diagnostic}"
        );
        assert!(
            !diagnostic.contains("real login failure"),
            "a denied keychain must never be reported as reachable: {diagnostic}"
        );
    }
}

#[test]
fn keychain_auth_diagnostic_stays_silent_outside_its_exact_failure_shape() {
    let sandbox = sandbox_for_test();
    let failure = "Failed to authenticate: OAuth session expired and could not be refreshed";
    let home = std::ffi::OsStr::new("/Users/test");

    // Providers that never read the keychain, unsandboxed runs, and unrelated
    // failures must not collect a keychain explanation.
    assert!(
        macos_keychain_auth_diagnostic_with("codex", Some(&sandbox), failure, Some(home)).is_none()
    );
    assert!(macos_keychain_auth_diagnostic_with("claude", None, failure, Some(home)).is_none());
    assert!(
        macos_keychain_auth_diagnostic_with(
            "claude",
            Some(&linux_sandbox_for_test(false)),
            failure,
            Some(home)
        )
        .is_none()
    );
    assert!(
        macos_keychain_auth_diagnostic_with(
            "claude",
            Some(&sandbox),
            "error: request timed out",
            Some(home)
        )
        .is_none()
    );
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

fn linux_sandbox_for_test(allow_fallback: bool) -> ResolvedSandbox {
    ResolvedSandbox {
        kind: ExecutorSandboxKind::LinuxBwrap,
        allow_fallback,
        ..sandbox_for_test()
    }
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
        (ErrorKind::WouldBlock, false),  // EAGAIN
        (ErrorKind::OutOfMemory, false), // ENOMEM
        (ErrorKind::Interrupted, false), // EINTR
        (ErrorKind::Other, false),       // unknown → conservative: retryable
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
fn provider_launcher_resolution_falls_back_to_temp_home_with_scrubbed_path() {
    let temp = tempdir().expect("tempdir");
    let fake_path = temp.path().join("system-bin");
    let home = temp.path().join("home");
    let provider_bin = home.join(".local/bin");
    std::fs::create_dir_all(&fake_path).expect("create fake PATH");
    std::fs::create_dir_all(&provider_bin).expect("create provider bin");
    let launcher = provider_bin.join("claude");
    write_executable(&launcher, "#!/bin/sh\nexit 0\n");

    let resolved = resolve_provider_launcher_with(
        "claude",
        "claude",
        Some(fake_path.as_os_str()),
        Some(&home),
        None,
    )
    .expect("HOME fallback should resolve provider");

    assert_eq!(resolved, launcher.to_string_lossy());
}

#[test]
fn missing_provider_launcher_error_names_provider_and_searched_locations() {
    let temp = tempdir().expect("tempdir");
    let fake_path = temp.path().join("system-bin");
    let home = temp.path().join("home");
    std::fs::create_dir_all(&fake_path).expect("create fake PATH");
    std::fs::create_dir_all(&home).expect("create fake HOME");

    let error = resolve_provider_launcher_with(
        "claude",
        "claude",
        Some(fake_path.as_os_str()),
        Some(&home),
        None,
    )
    .expect_err("missing launcher must fail");

    assert!(error.permanent, "missing launcher must remain permanent");
    assert!(
        error.message.contains("provider `claude`"),
        "error should name the provider: {}",
        error.message
    );
    for searched in [
        fake_path.join("claude"),
        home.join(".local/bin/claude"),
        home.join(".orbit/bin/claude"),
        home.join(".cargo/bin/claude"),
        home.join("bin/claude"),
        std::path::PathBuf::from("/opt/homebrew/bin/claude"),
        std::path::PathBuf::from("/usr/local/bin/claude"),
    ] {
        assert!(
            error.message.contains(&searched.display().to_string()),
            "error should name searched location {}: {}",
            searched.display(),
            error.message
        );
    }
    assert_eq!(
        SUPPORTED_SYSTEM_BIN_DIRS,
        &["/opt/homebrew/bin", "/usr/local/bin"]
    );
}

#[test]
fn agent_tool_environment_prefers_dispatching_orbit_over_stale_path_entry() {
    let inherited = std::env::join_paths(["/home/test/.cargo/bin", "/usr/bin"])
        .expect("construct inherited PATH");
    let env = orbit_tool_env_with(
        None,
        std::path::Path::new("/home/test/.orbit/bin/orbit"),
        Some(&inherited),
        None,
    )
    .expect("pin dispatching Orbit");

    assert_eq!(
        env[0],
        (
            "ORBIT_BIN".to_string(),
            "/home/test/.orbit/bin/orbit".to_string()
        )
    );
    let pinned = env
        .iter()
        .find(|(name, _)| name == "PATH")
        .map(|(_, value)| value)
        .expect("PATH override");
    assert_eq!(
        std::env::split_paths(std::ffi::OsStr::new(pinned)).collect::<Vec<_>>(),
        vec![
            std::path::PathBuf::from("/home/test/.orbit/bin"),
            std::path::PathBuf::from("/home/test/.cargo/bin"),
            std::path::PathBuf::from("/usr/bin"),
            std::path::PathBuf::from("/opt/homebrew/bin"),
            std::path::PathBuf::from("/usr/local/bin"),
        ]
    );
}

#[test]
fn configured_orbit_bin_wins_and_its_path_entry_is_deduplicated() {
    let inherited = std::env::join_paths(["/home/test/.cargo/bin", "/opt/orbit/bin", "/usr/bin"])
        .expect("construct inherited PATH");
    let env = orbit_tool_env_with(
        Some(std::ffi::OsStr::new("/opt/orbit/bin/orbit")),
        std::path::Path::new("/home/test/.orbit/bin/orbit"),
        Some(&inherited),
        None,
    )
    .expect("pin configured Orbit");

    assert_eq!(
        env[0],
        ("ORBIT_BIN".to_string(), "/opt/orbit/bin/orbit".to_string())
    );
    let pinned = &env[1].1;
    assert_eq!(
        std::env::split_paths(std::ffi::OsStr::new(pinned)).collect::<Vec<_>>(),
        vec![
            std::path::PathBuf::from("/opt/orbit/bin"),
            std::path::PathBuf::from("/home/test/.cargo/bin"),
            std::path::PathBuf::from("/usr/bin"),
            std::path::PathBuf::from("/opt/homebrew/bin"),
            std::path::PathBuf::from("/usr/local/bin"),
        ]
    );
}

#[test]
fn agent_tool_environment_backfills_conventional_home_bin_dirs_missing_from_path() {
    let inherited = std::env::join_paths(["/usr/bin"]).expect("construct inherited PATH");
    let home = std::path::Path::new("/home/test");
    let env = orbit_tool_env_with(
        None,
        std::path::Path::new("/home/test/.orbit/bin/orbit"),
        Some(&inherited),
        Some(home),
    )
    .expect("pin dispatching Orbit");

    let pinned = env
        .iter()
        .find(|(name, _)| name == "PATH")
        .map(|(_, value)| value)
        .expect("PATH override");
    assert_eq!(
        std::env::split_paths(std::ffi::OsStr::new(pinned)).collect::<Vec<_>>(),
        vec![
            std::path::PathBuf::from("/home/test/.orbit/bin"),
            std::path::PathBuf::from("/usr/bin"),
            std::path::PathBuf::from("/home/test/.local/bin"),
            std::path::PathBuf::from("/home/test/.cargo/bin"),
            std::path::PathBuf::from("/home/test/bin"),
            std::path::PathBuf::from("/opt/homebrew/bin"),
            std::path::PathBuf::from("/usr/local/bin"),
        ]
    );
}

/// launchd's default PATH on macOS. Drain, pilot, and shipment share
/// `resolve_provider_launcher`; this is the scheduled-worker environment
/// that failed to find `/opt/homebrew/bin/codex` on the live Mac cargo
/// binary `/Users/daniel/.cargo/bin/orbit` (0.19.2). Source HEAD at
/// pickup: `bca1cddc99987506c96e0ea626902f537d53572b`. [ORB-11808]
const MACOS_LAUNCHD_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

fn homebrew_style_prefix(root: &std::path::Path) -> std::path::PathBuf {
    root.join("opt").join("homebrew").join("bin")
}

fn launch_resolved_provider(program: &str) -> String {
    let spawned = spawn_bare(
        program,
        &[],
        &[("PATH".to_string(), MACOS_LAUNCHD_PATH.to_string())],
        None,
    )
    .expect("spawn resolved provider launcher");
    let output = spawned
        .child
        .wait_with_output()
        .expect("wait for resolved provider");
    assert!(
        output.status.success(),
        "resolved launcher must exit 0: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[cfg(unix)]
#[test]
fn minimal_macos_path_resolves_homebrew_style_provider_for_scheduled_and_interactive_launch() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let prefix = homebrew_style_prefix(temp.path());
    std::fs::create_dir_all(&home).expect("create fake HOME");
    std::fs::create_dir_all(&prefix).expect("create Homebrew-style prefix");
    let launcher = prefix.join("codex");
    write_executable(&launcher, "#!/bin/sh\nprintf 'codex-fixture-0.153.4\\n'\n");

    let scheduled = resolve_provider_launcher_with_extra_dirs(
        "codex",
        "codex",
        Some(std::ffi::OsStr::new(MACOS_LAUNCHD_PATH)),
        Some(&home),
        None,
        [prefix.clone()],
    )
    .expect("scheduled drain PATH must resolve Homebrew-style launcher");
    let interactive_path = format!("{MACOS_LAUNCHD_PATH}:{}", prefix.display());
    let interactive = resolve_provider_launcher_with_extra_dirs(
        "codex",
        "codex",
        Some(std::ffi::OsStr::new(&interactive_path)),
        Some(&home),
        None,
        [prefix.clone()],
    )
    .expect("interactive PATH must resolve Homebrew-style launcher");
    let pilot = resolve_provider_launcher_with_extra_dirs(
        "codex",
        "codex",
        Some(std::ffi::OsStr::new(MACOS_LAUNCHD_PATH)),
        Some(&home),
        None,
        [prefix],
    )
    .expect("pilot PATH must resolve Homebrew-style launcher");

    assert_eq!(scheduled, launcher.to_string_lossy());
    assert_eq!(interactive, scheduled);
    assert_eq!(pilot, scheduled);

    let scheduled_out = launch_resolved_provider(&scheduled);
    let interactive_out = launch_resolved_provider(&interactive);
    assert_eq!(scheduled_out, "codex-fixture-0.153.4\n");
    assert_eq!(interactive_out, scheduled_out);
}

#[cfg(unix)]
#[test]
fn explicit_launcher_path_wins_over_homebrew_style_prefix() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let prefix = homebrew_style_prefix(temp.path());
    let override_dir = temp.path().join("override");
    std::fs::create_dir_all(&home).expect("create fake HOME");
    std::fs::create_dir_all(&prefix).expect("create Homebrew-style prefix");
    std::fs::create_dir_all(&override_dir).expect("create override dir");
    write_executable(&prefix.join("codex"), "#!/bin/sh\nprintf 'homebrew\\n'\n");
    let override_launcher = override_dir.join("codex");
    write_executable(&override_launcher, "#!/bin/sh\nprintf 'override\\n'\n");

    let resolved = resolve_provider_launcher_with_extra_dirs(
        "codex",
        override_launcher.to_str().expect("utf-8 override path"),
        Some(std::ffi::OsStr::new(MACOS_LAUNCHD_PATH)),
        Some(&home),
        None,
        [prefix],
    )
    .expect("explicit path must be preserved");

    assert_eq!(resolved, override_launcher.to_string_lossy());
    assert_eq!(launch_resolved_provider(&resolved), "override\n");
}

#[cfg(unix)]
#[test]
fn path_executable_wins_over_homebrew_style_fallback() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let prefix = homebrew_style_prefix(temp.path());
    let path_dir = temp.path().join("path-bin");
    std::fs::create_dir_all(&home).expect("create fake HOME");
    std::fs::create_dir_all(&prefix).expect("create Homebrew-style prefix");
    std::fs::create_dir_all(&path_dir).expect("create PATH dir");
    write_executable(&prefix.join("codex"), "#!/bin/sh\nprintf 'homebrew\\n'\n");
    let path_launcher = path_dir.join("codex");
    write_executable(&path_launcher, "#!/bin/sh\nprintf 'path\\n'\n");

    let resolved = resolve_provider_launcher_with_extra_dirs(
        "codex",
        "codex",
        Some(path_dir.as_os_str()),
        Some(&home),
        None,
        [prefix],
    )
    .expect("PATH must beat Homebrew-style fallback");

    assert_eq!(resolved, path_launcher.to_string_lossy());
    assert_eq!(launch_resolved_provider(&resolved), "path\n");
}

#[cfg(unix)]
#[test]
fn non_executable_homebrew_style_file_is_skipped_and_absent_launcher_stays_permanent() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let prefix = homebrew_style_prefix(temp.path());
    std::fs::create_dir_all(&home).expect("create fake HOME");
    std::fs::create_dir_all(&prefix).expect("create Homebrew-style prefix");
    let stub = prefix.join("codex");
    std::fs::write(&stub, "#!/bin/sh\nprintf 'not-executable\\n'\n")
        .expect("write non-executable stub");

    let error = resolve_provider_launcher_with_extra_dirs(
        "codex",
        "codex",
        Some(std::ffi::OsStr::new(MACOS_LAUNCHD_PATH)),
        Some(&home),
        None,
        [prefix.clone()],
    )
    .expect_err("non-executable stub must not resolve");

    assert!(error.permanent, "missing launcher must remain permanent");
    assert!(
        error.message.contains("provider `codex`"),
        "error should name the provider: {}",
        error.message
    );
    assert!(
        error.message.contains(&stub.display().to_string()),
        "error should name the non-executable candidate: {}",
        error.message
    );
    assert!(
        error.message.contains("was not found"),
        "error should stay a missing-launcher diagnostic: {}",
        error.message
    );
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
