use super::super::spawn::{
    MAX_CACHED_PROFILES, MacosSandboxSpawnRequest, TEST_PROFILE_CACHE_LOCK,
    cached_profile_tempfile, profile_cache_len, spawn_under_macos_sandbox,
};
#[cfg(target_os = "macos")]
use super::super::test_support::{
    EnvOverrides, NEUTRAL_PROVIDER, compile_with_env, profile, sandbox_exec_can_apply, shell_escape,
};
use std::path::Path;
use std::process::Stdio;
use std::sync::PoisonError;

#[cfg(target_os = "macos")]
#[test]
fn spawn_under_macos_sandbox_ignores_fake_sandbox_exec_on_path() {
    // Every spawn inserts into the process-wide profile cache that the
    // LRU tests below measure.
    let _lock = TEST_PROFILE_CACHE_LOCK
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    if !sandbox_exec_can_apply() {
        return;
    }

    let temp = tempfile::tempdir().expect("tempdir");
    let fake_dir = temp.path().join("fake-bin");
    std::fs::create_dir_all(&fake_dir).expect("fake dir");
    let marker = temp.path().join("fake-used");
    let fake = fake_dir.join("sandbox-exec");
    std::fs::write(
        &fake,
        format!(
            "#!/bin/sh\necho fake > {}\nexit 77\n",
            shell_escape(&marker)
        ),
    )
    .expect("write fake sandbox-exec");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755))
            .expect("fake perms");
    }

    let poisoned_path = format!("{}:/usr/bin:/bin", fake_dir.display());
    let args = ["-c".to_string(), "exit 0".to_string()];
    let env = [("PATH".to_string(), poisoned_path)];
    let (child, _profile_file) = spawn_under_macos_sandbox(MacosSandboxSpawnRequest {
        profile_text: "(version 1)\n(allow default)\n",
        program: "/bin/sh",
        args: &args,
        env: &env,
        cwd: None,
        stdin: Stdio::null(),
        stdout: Stdio::piped(),
        stderr: Stdio::piped(),
        inherited_fds: &[],
    })
    .expect("spawn sandboxed child");
    let output = child.wait_with_output().expect("wait for child");

    assert!(
        output.status.success(),
        "trusted sandbox-exec should run child despite fake PATH entry; stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !marker.exists(),
        "fake sandbox-exec on PATH should not have been executed"
    );
}

/// [ORB-10917] `sandbox-exec` hands its own environment to the confined
/// program, so the launcher must apply exactly the environment the dispatcher
/// composed. The ambient variables are set here rather than read from the
/// developer's shell, and none carries a credential-shaped name — a denylist
/// would forward every one of them.
#[cfg(target_os = "macos")]
#[test]
fn spawn_under_macos_sandbox_gives_the_child_only_the_supplied_environment() {
    // Every spawn inserts into the process-wide profile cache that the
    // LRU tests below measure.
    let _lock = TEST_PROFILE_CACHE_LOCK
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    if !sandbox_exec_can_apply() {
        return;
    }

    let _ambient = orbit_common::test_env::scoped([
        ("DATABASE_URL", Some("postgres://svc:hunter2@db.internal")),
        ("BILLING_ENDPOINT", Some("https://billing.internal.example")),
        ("ORB_10917_AMBIENT", Some("leaked")),
        ("ANTHROPIC_API_KEY", Some("sk-ant-000000000000000000000")),
    ]);
    let args = ["-c".to_string(), "env".to_string()];
    let env = [
        ("PATH".to_string(), "/usr/bin:/bin".to_string()),
        ("ORB_10917_SUPPLIED".to_string(), "present".to_string()),
    ];
    let (child, _profile_file) = spawn_under_macos_sandbox(MacosSandboxSpawnRequest {
        profile_text: "(version 1)\n(allow default)\n",
        program: "/bin/sh",
        args: &args,
        env: &env,
        cwd: None,
        stdin: Stdio::null(),
        stdout: Stdio::piped(),
        stderr: Stdio::piped(),
        inherited_fds: &[],
    })
    .expect("spawn sandboxed child");
    let output = child.wait_with_output().expect("wait for child");
    let child_env = String::from_utf8_lossy(&output.stdout);

    assert!(
        child_env.contains("ORB_10917_SUPPLIED=present"),
        "supplied vars must reach the confined child: {child_env}"
    );
    for leaked in [
        "DATABASE_URL",
        "BILLING_ENDPOINT",
        "ORB_10917_AMBIENT",
        "ANTHROPIC_API_KEY",
    ] {
        assert!(
            !child_env.contains(leaked),
            "{leaked} must not reach a sandbox-exec provider child: {child_env}"
        );
    }
}

/// [ORB-12470] Regression probe for the missing `(allow pseudo-tty)` clause.
/// Runs under the real compiled profile (not a permissive `(allow default)`
/// test profile) so the assertion exercises exactly what a worker's own PTY
/// smoke test would hit. Never run directly by `cargo test`; spawned by
/// [`pty_allocation_is_allowed_under_compiled_profile`] as a sandboxed child
/// via its `--exact` module path, so renaming this test without updating that
/// caller fails the parent's child-test execution guard.
#[cfg(target_os = "macos")]
#[test]
#[ignore = "spawned under sandbox-exec by pty_allocation_is_allowed_under_compiled_profile"]
fn pty_probe_process() {
    let mut controller: libc::c_int = -1;
    let mut follower: libc::c_int = -1;
    let rc = unsafe {
        libc::openpty(
            &mut controller,
            &mut follower,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(
        rc,
        0,
        "openpty failed under the compiled sandbox profile: {}",
        std::io::Error::last_os_error()
    );
    unsafe {
        libc::close(controller);
        libc::close(follower);
    }
}

#[cfg(target_os = "macos")]
#[test]
fn pty_allocation_is_allowed_under_compiled_profile() {
    // Every spawn inserts into the process-wide profile cache that the
    // LRU tests below measure.
    let _lock = TEST_PROFILE_CACHE_LOCK
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    if !sandbox_exec_can_apply() {
        return;
    }

    let resolved = profile("pty-probe", &[], &[]);
    let profile_text = compile_with_env(&resolved, NEUTRAL_PROVIDER, EnvOverrides::default());
    let current_exe = std::env::current_exe().expect("current test binary path");
    let args = [
        "--exact".to_string(),
        "macos_sandbox::tests::spawn::pty_probe_process".to_string(),
        "--ignored".to_string(),
    ];
    let env = [("PATH".to_string(), "/usr/bin:/bin".to_string())];
    let (child, _profile_file) = spawn_under_macos_sandbox(MacosSandboxSpawnRequest {
        profile_text: &profile_text,
        program: current_exe.to_str().expect("utf8 test binary path"),
        args: &args,
        env: &env,
        cwd: None,
        stdin: Stdio::null(),
        stdout: Stdio::piped(),
        stderr: Stdio::piped(),
        inherited_fds: &[],
    })
    .expect("spawn sandboxed pty probe");
    let output = child.wait_with_output().expect("wait for pty probe");

    orbit_common::test_env::assert_child_test_passed(
        "macos_sandbox::tests::spawn::pty_probe_process",
        output.status,
        &output.stdout,
        &output.stderr,
    );
}

/// [ORB-13454] Inactive profile tempfile retention must stay bounded to
/// [`MAX_CACHED_PROFILES`] and evicted inactive tempfiles must be reclaimed
/// from disk when more distinct profiles are requested than the cache bound.
#[test]
fn cached_profile_tempfiles_stay_bounded_and_evict_inactive_entries() {
    let _lock = TEST_PROFILE_CACHE_LOCK
        .lock()
        .unwrap_or_else(PoisonError::into_inner);

    let p0 = cached_profile_tempfile("(version 1)\n(allow default)\n; orb-13454-bound-0\n")
        .expect("cache profile 0");
    let path0 = p0.path().to_path_buf();
    assert!(
        path0.exists(),
        "initial profile tempfile should exist on disk"
    );

    // Drop the strong Arc handle; the file is now inactive and retained only by the cache.
    drop(p0);
    assert!(
        path0.exists(),
        "inactive profile must remain in cache until evicted"
    );

    // Request MAX_CACHED_PROFILES distinct profiles, each dropped immediately so they are inactive.
    for i in 1..=MAX_CACHED_PROFILES {
        let profile_text = format!("(version 1)\n(allow default)\n; orb-13454-bound-{i}\n");
        let p = cached_profile_tempfile(&profile_text).expect("cache profile");
        assert!(p.path().exists());
    }

    // Since MAX_CACHED_PROFILES + 1 distinct profiles were requested, p0 must have been evicted.
    // Because no active caller retains p0, the tempfile descriptor and disk file must be reclaimed.
    assert!(
        !path0.exists(),
        "evicted inactive profile tempfile must be unlinked and reclaimed from disk"
    );
    assert!(
        profile_cache_len() <= MAX_CACHED_PROFILES,
        "profile cache size must stay bounded to MAX_CACHED_PROFILES (len={})",
        profile_cache_len()
    );
}

/// [ORB-13454] Active consumers holding [`Arc<NamedTempFile>`] retain their
/// profile files on disk even after eviction from the cache, and the file is
/// reclaimed as soon as the active handle drops.
#[test]
fn profiles_referenced_by_active_callers_remain_available_until_dropped() {
    let _lock = TEST_PROFILE_CACHE_LOCK
        .lock()
        .unwrap_or_else(PoisonError::into_inner);

    let active_profile =
        cached_profile_tempfile("(version 1)\n(allow default)\n; orb-13454-active-retained\n")
            .expect("cache active profile");
    let active_path = active_profile.path().to_path_buf();
    assert!(
        active_path.exists(),
        "active profile tempfile should exist on disk"
    );

    // Evict active_profile from the cache by inserting MAX_CACHED_PROFILES distinct profiles.
    for i in 1..=MAX_CACHED_PROFILES {
        let profile_text = format!("(version 1)\n(allow default)\n; orb-13454-active-evict-{i}\n");
        let _ = cached_profile_tempfile(&profile_text).expect("cache profile");
    }

    // Even though active_profile was evicted from the process-wide cache, the caller's Arc
    // reference must keep the file alive and readable for the child's lifetime.
    assert!(
        active_path.exists(),
        "profile still referenced by active caller must remain available on disk"
    );
    let content = std::fs::read_to_string(&active_path).expect("read active profile file");
    assert!(
        content.contains("; orb-13454-active-retained"),
        "active profile content must be intact"
    );

    // Once the active caller drops its handle, the temporary file must be reclaimed.
    drop(active_profile);
    assert!(
        !active_path.exists(),
        "profile tempfile must be reclaimed once active handle is dropped"
    );
}

/// [ORB-13454] Repeated failed spawns cannot grow inactive profile retention
/// without bound.
#[test]
fn repeated_failed_spawns_cannot_grow_inactive_profile_retention_without_bound() {
    let _lock = TEST_PROFILE_CACHE_LOCK
        .lock()
        .unwrap_or_else(PoisonError::into_inner);

    // A missing working directory makes `Command::spawn` itself fail on every
    // host. A nonexistent program alone does not: where `sandbox-exec` exists,
    // it spawns fine and only the confined exec fails later.
    let missing_cwd = Path::new("/nonexistent/orbit-test-fail-spawn-cwd");

    // First failed spawn creates and caches a profile tempfile before failing.
    let first_profile = "(version 1)\n(allow default)\n; orb-13454-failed-spawn-0\n";
    let first_res = spawn_under_macos_sandbox(MacosSandboxSpawnRequest {
        profile_text: first_profile,
        program: "/nonexistent/binary/orbit-test-fail-spawn",
        args: &[],
        env: &[],
        cwd: Some(missing_cwd),
        stdin: Stdio::null(),
        stdout: Stdio::piped(),
        stderr: Stdio::piped(),
        inherited_fds: &[],
    });
    assert!(
        first_res.is_err(),
        "spawn with nonexistent binary should fail"
    );

    // The profile tempfile was cached prior to the spawn failure.
    let first_cached = cached_profile_tempfile(first_profile).expect("get cached first profile");
    let first_path = first_cached.path().to_path_buf();
    drop(first_cached);
    assert!(
        first_path.exists(),
        "failed spawn profile tempfile was cached and exists on disk"
    );

    // Perform more failed spawns than the cache capacity.
    for i in 1..=(MAX_CACHED_PROFILES + 5) {
        let profile_text = format!("(version 1)\n(allow default)\n; orb-13454-failed-spawn-{i}\n");
        let res = spawn_under_macos_sandbox(MacosSandboxSpawnRequest {
            profile_text: &profile_text,
            program: "/nonexistent/binary/orbit-test-fail-spawn",
            args: &[],
            env: &[],
            cwd: Some(missing_cwd),
            stdin: Stdio::null(),
            stdout: Stdio::piped(),
            stderr: Stdio::piped(),
            inherited_fds: &[],
        });
        assert!(res.is_err(), "spawn with nonexistent binary should fail");
    }

    // The first failed spawn profile must have been evicted and reclaimed.
    assert!(
        !first_path.exists(),
        "first failed spawn profile tempfile must have been evicted and reclaimed"
    );
    assert!(
        profile_cache_len() <= MAX_CACHED_PROFILES,
        "profile cache size must remain bounded after repeated failed spawns (len={})",
        profile_cache_len()
    );
}
